//! 隧道管理器。M2 完整实现：本地 -L / 动态 SOCKS5 -D / 远程 -R。
//!
//! 架构（规格书第 6/8/10 条 + docs/design/05、07）：
//! - 独立线程 + 独立 tokio runtime（2 worker），隧道负载不占交互链路调度；
//! - 数据路径零 IPC：TcpStream ↔ direct-tcpip ChannelStream 直接中继，32KB 缓冲；
//! - 全链路有界背压：每连接 2×32KB、活跃连接数上限、accept 暂停即排队（TCP backlog）；
//! - 断线重连：监督器持连接槽（watch），死亡/显式通知 → 指数退避重建；
//!   重连期间新到连接在槽上等 Connected（Queue）或直接拒（FailFast）。

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Notify};

use core_ssh::SshConnection;

use crate::error::TunnelError;

/// 中继缓冲：对齐 channel max_packet_size（spike 踩坑 #5）
const RELAY_BUF: usize = 32 * 1024;
/// 等待连接重建的上限（Queue 策略）
const RECONNECT_WAIT: Duration = Duration::from_secs(30);
/// 连接活性探测周期
const LIVENESS_POLL: Duration = Duration::from_secs(2);

/// 连接工厂：app 层注入（sessionId → Bulk ConnectOptions），core-tunnel 不感知 store
pub type ConnectFn = Arc<
    dyn Fn() -> std::pin::Pin<
            Box<dyn Future<Output = Result<SshConnection, core_ssh::SshError>> + Send>,
        > + Send
        + Sync,
>;

/// 重连期间新到本地连接的策略（规格书：不得静默丢弃，不得无限堆积）
#[derive(Debug, Clone, Copy)]
pub enum DisconnectPolicy {
    /// 在连接槽上等 Connected（上限 RECONNECT_WAIT），OS backlog 天然有界
    Queue,
    /// 立即失败（accept 即关闭）
    FailFast,
}

#[derive(Debug, Clone)]
pub enum TunnelKind {
    /// 本地 -L
    Local { bind: (String, u16) },
    /// 远程 -R
    Remote { bind: (String, u16) },
    /// 动态 SOCKS5 -D
    DynamicSocks5 { bind: (String, u16) },
}

impl TunnelKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::Remote { .. } => "remote",
            Self::DynamicSocks5 { .. } => "dynamic",
        }
    }
    pub fn bind(&self) -> &(String, u16) {
        match self {
            Self::Local { bind } | Self::Remote { bind } | Self::DynamicSocks5 { bind } => bind,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TunnelSpec {
    pub kind: TunnelKind,
    /// Local/Remote 必填：转发目标
    pub target: Option<(String, u16)>,
    pub max_conns: u64,
    pub on_disconnect: DisconnectPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelStatus {
    Starting,
    Listening,
    Reconnecting,
    Stopped,
    Failed,
}

/// 原子计数器（数据路径只写这些；1Hz 快照由 app 轮询差分得速率）
#[derive(Default)]
pub struct StatsAtomic {
    pub active_conns: AtomicU64,
    pub total_conns: AtomicU64,
    pub bytes_up: AtomicU64,
    pub bytes_down: AtomicU64,
    pub errors: AtomicU64,
    pub reconnects: AtomicU32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TunnelStats {
    pub active_conns: u64,
    pub total_conns: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub errors: u64,
    pub reconnects: u32,
}

#[derive(Debug, Clone)]
pub struct TunnelInfo {
    pub id: String,
    pub kind: String,
    pub bind: String,
    pub target: Option<String>,
    pub status: TunnelStatus,
    pub stats: TunnelStats,
    /// 最近一次连接/运行错误文本（无错误为 None）
    pub last_error: Option<String>,
}

struct TunnelEntry {
    status: Arc<Mutex<TunnelStatus>>,
    stats: Arc<StatsAtomic>,
    last_error: Arc<Mutex<Option<String>>>,
    shutdown: watch::Sender<bool>,
    kind_label: &'static str,
    bind: String,
    target: Option<String>,
}

enum Slot {
    Connecting,
    Connected(Arc<SshConnection>),
}

/// 全局单例 runtime 在第一个隧道启动时建立
pub struct TunnelManager {
    tunnels: Mutex<HashMap<String, Arc<TunnelEntry>>>,
    rt: tokio::runtime::Handle,
    _rt_thread: std::thread::JoinHandle<()>,
}

impl TunnelManager {
    /// 独立线程 + 独立 runtime（规格书第 8 条）
    pub fn new() -> Arc<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<tokio::runtime::Handle>();
        let thread = std::thread::Builder::new()
            .name("tunnel-rt".into())
            // 线程创建失败 = 进程资源枯竭级故障，fail loud
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("tunnel-worker")
                    .build();
                match rt {
                    Ok(rt) => {
                        let _ = tx.send(rt.handle().clone());
                        rt.block_on(std::future::pending::<()>());
                    }
                    Err(e) => {
                        tracing::error!(?e, "tunnel runtime build failed");
                    }
                }
            })
            .unwrap_or_else(|e| panic!("spawn tunnel-rt thread: {e}"));
        let rt = rx
            .recv()
            .unwrap_or_else(|_| panic!("tunnel runtime failed to start (handle channel closed)"));
        Arc::new(Self {
            tunnels: Mutex::new(HashMap::new()),
            rt,
            _rt_thread: thread,
        })
    }

    pub async fn start(
        self: &Arc<Self>,
        id: String,
        spec: TunnelSpec,
        connect: ConnectFn,
    ) -> Result<(), TunnelError> {
        if self.tunnels.lock().contains_key(&id) {
            return Err(TunnelError::Listen {
                bind: id.clone(),
                reason: "隧道 id 已存在".into(),
            });
        }
        // 监听 socket 在调用侧建立：绑定错误同步返回（E4001）；
        // 端口 0 时用实际占用端口回填（瞬态端口支持）
        let (listener, bind_label) = match &spec.kind {
            TunnelKind::Local { bind } | TunnelKind::DynamicSocks5 { bind } => {
                let l = bind_listener(bind)?;
                let label = match l.local_addr() {
                    Ok(addr) => format!("{}:{}", addr.ip(), addr.port()),
                    Err(_) => format!("{}:{}", bind.0, bind.1),
                };
                (Some(l), label)
            }
            TunnelKind::Remote { bind } => (None, format!("{}:{}", bind.0, bind.1)),
        };

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let stats = Arc::new(StatsAtomic::default());
        let entry = Arc::new(TunnelEntry {
            status: Arc::new(Mutex::new(TunnelStatus::Starting)),
            stats: stats.clone(),
            last_error: Arc::new(Mutex::new(None)),
            shutdown: shutdown_tx,
            kind_label: spec.kind.label(),
            bind: bind_label,
            target: spec.target.as_ref().map(|(h, p)| format!("{h}:{p}")),
        });

        let task_spec = spec.clone();
        let task_entry = entry.clone();
        let done_entry = entry.clone();
        self.rt.spawn(async move {
            let result = match task_spec.kind.clone() {
                TunnelKind::Local { .. } | TunnelKind::DynamicSocks5 { .. } => match listener {
                    Some(l) => run_listener(task_spec, connect, l, task_entry, shutdown_rx).await,
                    None => Err(TunnelError::Listen {
                        bind: "internal".into(),
                        reason: "本地/动态隧道缺监听 socket".into(),
                    }),
                },
                TunnelKind::Remote { bind } => {
                    run_remote(task_spec, connect, bind, task_entry, shutdown_rx).await
                }
            };
            if let Err(e) = result {
                // 致命退出：Failed 终态 + 记录错误文本（此前状态滞留为 Starting/Reconnecting）
                *done_entry.status.lock() = TunnelStatus::Failed;
                *done_entry.last_error.lock() = Some(e.to_string());
                tracing::warn!(error = %e, "tunnel task exited with error");
            }
        });

        self.tunnels.lock().insert(id, entry);
        Ok(())
    }

    pub async fn stop(&self, id: &str) -> Result<(), TunnelError> {
        let entry = self.tunnels.lock().remove(id);
        match entry {
            Some(e) => {
                *e.status.lock() = TunnelStatus::Stopped;
                let _ = e.shutdown.send(true);
                Ok(())
            }
            None => Err(TunnelError::NotFound(id.into())),
        }
    }

    pub fn list(&self) -> Vec<TunnelInfo> {
        self.tunnels
            .lock()
            .iter()
            .map(|(id, e)| {
                let status = *e.status.lock();
                TunnelInfo {
                    id: id.clone(),
                    kind: e.kind_label.into(),
                    bind: e.bind.clone(),
                    target: e.target.clone(),
                    status,
                    stats: snapshot(&e.stats),
                    last_error: e.last_error.lock().clone(),
                }
            })
            .collect()
    }

    pub fn stats(&self, id: &str) -> Option<TunnelStats> {
        self.tunnels.lock().get(id).map(|e| snapshot(&e.stats))
    }
}

fn snapshot(s: &StatsAtomic) -> TunnelStats {
    TunnelStats {
        active_conns: s.active_conns.load(Ordering::Relaxed),
        total_conns: s.total_conns.load(Ordering::Relaxed),
        bytes_up: s.bytes_up.load(Ordering::Relaxed),
        bytes_down: s.bytes_down.load(Ordering::Relaxed),
        errors: s.errors.load(Ordering::Relaxed),
        reconnects: s.reconnects.load(Ordering::Relaxed),
    }
}

/// 大 backlog 监听（spike 踩坑 #6：默认 SOMAXCONN 在 500 并发 SYN 突发下溢出）
fn bind_listener(bind: &(String, u16)) -> Result<std::net::TcpListener, TunnelError> {
    use socket2::{Domain, SockAddr, Socket, Type};
    let bind_err = |reason: String| TunnelError::Listen {
        bind: format!("{}:{}", bind.0, bind.1),
        reason,
    };
    let addr: std::net::SocketAddr = format!("{}:{}", bind.0, bind.1)
        .parse()
        .map_err(|e: std::net::AddrParseError| bind_err(e.to_string()))?;
    let socket = Socket::new(Domain::for_address(addr), Type::STREAM, None)
        .map_err(|e| bind_err(e.to_string()))?;
    socket
        .set_nonblocking(true)
        .map_err(|e| bind_err(e.to_string()))?;
    // 注意：Windows 上 SO_REUSEADDR 允许同端口双绑（与 Unix 语义不同），
    // 会吞掉端口冲突——隧道监听不设；accepted 连接的 TIME_WAIT 与监听 socket 无关
    #[cfg(not(target_os = "windows"))]
    socket
        .set_reuse_address(true)
        .map_err(|e| bind_err(e.to_string()))?;
    socket
        .bind(&SockAddr::from(addr))
        .map_err(|e| bind_err(e.to_string()))?;
    socket.listen(4096).map_err(|e| bind_err(e.to_string()))?;
    Ok(socket.into())
}

/// 连接监督器：建连 → 活性监视（is_closed 轮询 + 显式通知）→ 退避重建
async fn supervise_conn(
    connect: ConnectFn,
    slot: watch::Sender<Slot>,
    notify: Arc<Notify>,
    status: Arc<Mutex<TunnelStatus>>,
    stats: Arc<StatsAtomic>,
    last_error: Arc<Mutex<Option<String>>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut attempt = 0u32;
    loop {
        if *shutdown.borrow() {
            return;
        }
        match connect().await {
            Ok(conn) => {
                attempt = 0;
                let conn = Arc::new(conn);
                *status.lock() = TunnelStatus::Listening;
                *last_error.lock() = None;
                let _ = slot.send(Slot::Connected(conn.clone()));
                // 活到死
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep(LIVENESS_POLL) => {
                            if conn.is_closed() { break; }
                        }
                        _ = notify.notified() => break,
                        _ = shutdown.changed() => return,
                    }
                }
                stats.reconnects.fetch_add(1, Ordering::Relaxed);
                *status.lock() = TunnelStatus::Reconnecting;
                let _ = slot.send(Slot::Connecting);
            }
            Err(e) => {
                attempt += 1;
                *last_error.lock() = Some(e.to_string());
                tracing::warn!(attempt, error = %e, "tunnel connect failed, backing off");
                let backoff = Duration::from_secs((1u64 << attempt.min(4)).min(15));
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.changed() => return,
                }
            }
        }
    }
}

/// 等连接槽变 Connected；Queue 有上限，FailFast 不等
async fn wait_connected(
    slot: &watch::Receiver<Slot>,
    policy: DisconnectPolicy,
) -> Option<Arc<SshConnection>> {
    if let Slot::Connected(c) = &*slot.borrow() {
        return Some(c.clone());
    }
    match policy {
        DisconnectPolicy::FailFast => None,
        DisconnectPolicy::Queue => {
            let mut rx = slot.clone();
            let wait = async move {
                loop {
                    rx.changed().await.ok()?;
                    if let Slot::Connected(c) = &*rx.borrow() {
                        return Some(c.clone());
                    }
                }
            };
            tokio::time::timeout(RECONNECT_WAIT, wait)
                .await
                .ok()
                .flatten()
        }
    }
}

async fn run_listener(
    spec: TunnelSpec,
    connect: ConnectFn,
    listener: std::net::TcpListener,
    entry: Arc<TunnelEntry>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), TunnelError> {
    let listener = TcpListener::from_std(listener).map_err(|e| TunnelError::Listen {
        bind: entry.bind.clone(),
        reason: e.to_string(),
    })?;
    let (slot_tx, slot_rx) = watch::channel(Slot::Connecting);
    let notify = Arc::new(Notify::new());
    tokio::spawn(supervise_conn(
        connect,
        slot_tx,
        notify.clone(),
        entry.status.clone(),
        entry.stats.clone(),
        entry.last_error.clone(),
        shutdown.clone(),
    ));

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accept = listener.accept() => {
                let Ok((tcp, _peer)) = accept else { break };
                let _ = tcp.set_nodelay(true);
                let stats = entry.stats.clone();
                if stats.active_conns.load(Ordering::Relaxed) >= spec.max_conns {
                    stats.errors.fetch_add(1, Ordering::Relaxed);
                    drop(tcp); // E4003 语义：超限拒收
                    continue;
                }
                stats.active_conns.fetch_add(1, Ordering::Relaxed);
                stats.total_conns.fetch_add(1, Ordering::Relaxed);
                let slot = slot_rx.clone();
                let notify = notify.clone();
                let kind = spec.kind.clone();
                let target = spec.target.clone();
                let policy = spec.on_disconnect;
                tokio::spawn(async move {
                    match kind {
                        TunnelKind::DynamicSocks5 { .. } => {
                            handle_socks5(tcp, slot, notify, policy, stats).await;
                        }
                        TunnelKind::Local { .. } => {
                            if let Some((host, port)) = target {
                                handle_local(tcp, slot, notify, policy, stats, host, port).await;
                            }
                        }
                        TunnelKind::Remote { .. } => unreachable!(),
                    }
                });
            }
        }
    }
    Ok(())
}

async fn handle_local(
    mut tcp: TcpStream,
    slot: watch::Receiver<Slot>,
    notify: Arc<Notify>,
    policy: DisconnectPolicy,
    stats: Arc<StatsAtomic>,
    host: String,
    port: u16,
) {
    // 开通道失败一次 → 通知重连 + 等槽位 → 重试一次；再败计数放弃
    for attempt in 0..2 {
        let Some(conn) = wait_connected(&slot, policy).await else {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            stats.active_conns.fetch_sub(1, Ordering::Relaxed);
            return;
        };
        match conn.open_direct_tcpip(&host, port).await {
            Ok(chan) => {
                relay(&mut tcp, chan, &stats).await;
                stats.active_conns.fetch_sub(1, Ordering::Relaxed);
                return;
            }
            Err(_) if attempt == 0 => notify.notify_one(),
            Err(_) => break,
        }
    }
    stats.errors.fetch_add(1, Ordering::Relaxed);
    stats.active_conns.fetch_sub(1, Ordering::Relaxed);
}

async fn handle_socks5(
    mut tcp: TcpStream,
    slot: watch::Receiver<Slot>,
    notify: Arc<Notify>,
    policy: DisconnectPolicy,
    stats: Arc<StatsAtomic>,
) {
    let target = match socks5_handshake(&mut tcp).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            stats.active_conns.fetch_sub(1, Ordering::Relaxed);
            return;
        }
        Err(_) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            stats.active_conns.fetch_sub(1, Ordering::Relaxed);
            return;
        }
    };
    handle_local(tcp, slot, notify, policy, stats, target.0, target.1).await;
}

/// SOCKS5 无认证 CONNECT：greeting → request → reply；返回目标地址
async fn socks5_handshake(tcp: &mut TcpStream) -> std::io::Result<Option<(String, u16)>> {
    // greeting: VER NMETHODS METHODS...
    let n = tcp.read_u8().await?;
    if n != 0x05 {
        return Ok(None);
    }
    let nmethods = tcp.read_u8().await? as usize;
    let mut methods = vec![0u8; nmethods];
    tcp.read_exact(&mut methods).await?;
    // 无需认证
    tcp.write_all(&[0x05, 0x00]).await?;

    // request: VER CMD RSV ATYP DST.ADDR DST.PORT
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await?;
    if head[0] != 0x05 || head[1] != 0x01 {
        return Ok(None); // 仅支持 CONNECT
    }
    let host = match head[3] {
        0x01 => {
            let mut a = [0u8; 4];
            tcp.read_exact(&mut a).await?;
            std::net::Ipv4Addr::from(a).to_string()
        }
        0x03 => {
            let len = tcp.read_u8().await? as usize;
            let mut d = vec![0u8; len];
            tcp.read_exact(&mut d).await?;
            String::from_utf8_lossy(&d).into_owned()
        }
        0x04 => {
            let mut a = [0u8; 16];
            tcp.read_exact(&mut a).await?;
            std::net::Ipv6Addr::from(a).to_string()
        }
        _ => return Ok(None),
    };
    let port = tcp.read_u16().await?;
    // reply: 成功（bind 地址填 0.0.0.0:0 占位）
    tcp.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await?;
    Ok(Some((host, port)))
}

/// 远程转发：注册 → 收 forwarded-tcpip 通道 → 连本地目标 → 中继
async fn run_remote(
    spec: TunnelSpec,
    connect: ConnectFn,
    bind: (String, u16),
    entry: Arc<TunnelEntry>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), TunnelError> {
    let Some((target_host, target_port)) = spec.target.clone() else {
        return Err(TunnelError::Listen {
            bind: entry.bind.clone(),
            reason: "远程转发缺目标".into(),
        });
    };
    let (slot_tx, slot_rx) = watch::channel(Slot::Connecting);
    let notify = Arc::new(Notify::new());
    tokio::spawn(supervise_conn(
        connect,
        slot_tx,
        notify,
        entry.status.clone(),
        entry.stats.clone(),
        entry.last_error.clone(),
        shutdown.clone(),
    ));

    'outer: loop {
        let Some(conn) = wait_connected(&slot_rx, DisconnectPolicy::Queue).await else {
            if *shutdown.borrow() {
                break;
            }
            continue;
        };
        let mut rx = match conn.tcpip_forward(&bind.0, bind.1).await {
            Ok(rx) => rx,
            Err(e) => {
                tracing::warn!(error = %e, "tcpip_forward failed");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                    _ = shutdown.changed() => break 'outer,
                }
                continue;
            }
        };
        *entry.status.lock() = TunnelStatus::Listening;
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    let _ = conn.cancel_tcpip_forward(&bind.0, bind.1).await;
                    break 'outer;
                }
                ch = rx.recv() => {
                    let Some(ch) = ch else { continue 'outer }; // 路由被移除（重连）→ 重新注册
                    let stats = entry.stats.clone();
                    let (th, tp) = (target_host.clone(), target_port);
                    tokio::spawn(async move {
                        stats.active_conns.fetch_add(1, Ordering::Relaxed);
                        stats.total_conns.fetch_add(1, Ordering::Relaxed);
                        match TcpStream::connect((th.as_str(), tp)).await {
                            Ok(mut tcp) => {
                                let _ = tcp.set_nodelay(true);
                                relay(&mut tcp, ch.into_stream(), &stats).await;
                            }
                            Err(_) => {
                                stats.errors.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        stats.active_conns.fetch_sub(1, Ordering::Relaxed);
                    });
                }
            }
        }
    }
    Ok(())
}

/// 双向中继：有界缓冲（每连接 2×32KB），channel window 耗尽时 write 挂起，
/// 停止从本地 socket 读取——背压沿链路传导（规格书第 10 条）
async fn relay(
    tcp: &mut TcpStream,
    chan: russh::ChannelStream<russh::client::Msg>,
    stats: &StatsAtomic,
) {
    let (mut cr, mut cw) = tokio::io::split(chan);
    let (mut tr, mut tw) = tcp.split();

    let up_stats = stats;
    let up = async move {
        let mut buf = vec![0u8; RELAY_BUF];
        loop {
            match tr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if cw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    up_stats.bytes_up.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
    };

    let down = async move {
        let mut buf = vec![0u8; RELAY_BUF];
        loop {
            match cr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    stats.bytes_down.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
    };

    // 任一方向结束即收尾
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}
