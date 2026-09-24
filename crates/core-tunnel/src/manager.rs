//! 隧道管理器。M2 完整实现：本地 -L / 动态 SOCKS5 -D / 远程 -R。
//!
//! 架构（规格书第 6/8/10 条 + docs/design/05、07）：
//! - 独立线程 + 独立 tokio runtime（2 worker），隧道负载不占交互链路调度；
//! - 数据路径零 IPC：TcpStream ↔ direct-tcpip ChannelStream 直接中继，32KB 固定数组缓冲；
//! - 全链路有界背压：每连接 2×32KB、活跃连接数上限、accept 暂停即排队（TCP backlog）；
//! - 断线重连：监督器持连接槽（watch），死亡/显式通知 → 指数退避重建；
//!   重连期间新到连接在槽上等 Connected（Queue）或直接拒（FailFast）；
//! - relay 任务所有权（PR-7 / C1 方案 A）：每隧道单 supervisor 独占 JoinSet，
//!   accept/forward 任务只经 mpsc 提交 relay 请求；stop 分阶段：停 accept →
//!   drain（stop_grace_timeout）→ 超时强制 abort → join 全部任务 →
//!   断言 active_conns 归零 → stop() 返回前才删 entry；
//! - relay 半关闭：本端 EOF → channel EOF，对端 EOF → TcpStream shutdown(Write)；
//!   一侧结束后另一侧允许排空（half_close_drain_timeout，从一侧 EOF 起算）；
//!   读重置/协议错误立即终止双向。

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch, Notify, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;

use core_ssh::SshConnection;

use crate::error::TunnelError;

/// 中继缓冲：对齐 channel max_packet_size（spike 踩坑 #5）
const RELAY_BUF: usize = 32 * 1024;
/// 等待连接重建的上限（Queue 策略）
const RECONNECT_WAIT: Duration = Duration::from_secs(30);
/// 连接活性探测周期
const LIVENESS_POLL: Duration = Duration::from_secs(2);
/// open_direct_tcpip 建通道超时（防对端不响应永久占用任务）
const CHANNEL_OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// 远程转发连本地目标超时
const TARGET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// SOCKS5 greeting+request 总超时（防慢握手占名额）
const SOCKS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// SOCKS5 握手并发上限（与 relay 配额分离：慢握手不得饿死正式连接）
const SOCKS_HANDSHAKE_CONCURRENCY: usize = 64;
/// relay 请求队列容量（accept/forward → supervisor；满载让 accept 变慢，
/// 背压自然传导到 OS backlog）
const RELAY_REQ_QUEUE: usize = 64;
/// EOF/shutdown 信号发送 deadline（半关闭收尾操作不得悬挂）
const EOF_SIGNAL_TIMEOUT: Duration = Duration::from_secs(5);
/// 默认 stop 排空宽限：用户停止时已有 relay 自然收尾的上限，超时强制 abort
pub const DEFAULT_STOP_GRACE_TIMEOUT: Duration = Duration::from_secs(5);
/// 默认半关闭排空上限：从一侧 EOF 起算，对侧无响应到点释放该 relay
pub const DEFAULT_HALF_CLOSE_DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

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
    /// 用户停止时 relay 排空宽限（超时强制 abort）；与 half_close_drain 独立
    pub stop_grace_timeout: Duration,
    /// 半关闭后排空上限：一侧 EOF 起算，对侧无响应到点释放该 relay
    pub half_close_drain_timeout: Duration,
    /// 归属会话：运行条目自持会话绑定（生命周期扇出按它停隧道，不读库定义）；
    /// 空串 = 独立隧道（测试/示例），不参与会话失效扇出
    pub session_id: String,
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
    /// 因并发上限被拒的连接数（独立于 errors：满载拒绝不是错误）
    pub rejected_conns: AtomicU64,
    pub reconnects: AtomicU32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TunnelStats {
    pub active_conns: u64,
    pub total_conns: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub errors: u64,
    pub rejected_conns: u64,
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
    /// 归属会话（TunnelSpec.session_id 原样；空串 = 独立隧道）
    pub session_id: String,
}

struct TunnelEntry {
    status: Arc<Mutex<TunnelStatus>>,
    stats: Arc<StatsAtomic>,
    last_error: Arc<Mutex<Option<String>>>,
    shutdown: watch::Sender<bool>,
    /// stop 已开始（幂等闸）：并发 stop 共享同一完成信号
    stop_started: AtomicBool,
    /// supervisor join 完全部任务后发送（send_replace：无接收者也生效）；
    /// stop() 等此信号才删 entry——返回时任务/连接已归零
    stopped: watch::Sender<bool>,
    /// stop() 等待总上限（grace + abort/join 余量）：防 supervisor 异常导致 stop 悬挂
    stop_deadline: Duration,
    kind_label: &'static str,
    bind: String,
    target: Option<String>,
    session_id: String,
}

enum Slot {
    Connecting,
    Connected(Arc<SshConnection>),
}

/// Transport channel 上限（PR-15 组级；C10 数据结构预留多 Transport 语义，
/// 第一期 max_transports=1 单槽，扩容依据指标再议）
/// 上限值单一事实源在 core_policy::budget::caps（PR-17 资源表）
const GROUP_CHANNEL_MAX: usize = core_policy::budget::caps::TUNNEL_CHANNEL;
/// channel permit 等待上限：超时明确拒绝（PR-15 第一期策略；
/// 单隧道 max_conns 配额（C5 立即拒）在其上仍然独立先生效）
const GROUP_CHANNEL_WAIT: Duration = Duration::from_secs(3);

/// SessionTunnelGroup（PR-15）：supervisor/slot/notify 从单隧道提升到组——
/// 同组隧道共享一条 Tunnel Transport（断线由组统一重建一次，根治惊群）。
/// 组生命周期 = 对组 lease（活跃隧道数）：归零即关停监督器并摘出注册表。
struct TunnelGroup {
    key: String,
    slot: watch::Sender<Slot>,
    notify: Arc<Notify>,
    status: Arc<Mutex<TunnelStatus>>,
    /// reconnects 归组：共享连接的重连不被每个隧道重复计
    stats: Arc<StatsAtomic>,
    last_error: Arc<Mutex<Option<String>>>,
    leases: AtomicU32,
    shutdown: watch::Sender<bool>,
    /// Transport channel 上限（PR-17 接入 Governor 账本：CAS 扣减 + 唤醒，禁止轮询；
    /// 与单隧道 relay 配额独立叠加）
    channel_budget: Arc<core_policy::Budget>,
}

/// 全局单例 runtime 在第一个隧道启动时建立
pub struct TunnelManager {
    tunnels: Mutex<HashMap<String, Arc<TunnelEntry>>>,
    /// SessionTunnelGroup 注册表（共享键 → 组；PR-15）
    groups: Mutex<HashMap<String, Arc<TunnelGroup>>>,
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
            groups: Mutex::new(HashMap::new()),
            rt,
            _rt_thread: thread,
        })
    }

    pub async fn start(
        self: &Arc<Self>,
        id: String,
        spec: TunnelSpec,
        group_key: String,
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
        let (stopped_tx, _stopped_rx) = watch::channel(false);
        let entry = Arc::new(TunnelEntry {
            status: Arc::new(Mutex::new(TunnelStatus::Starting)),
            stats: Arc::new(StatsAtomic::default()),
            last_error: Arc::new(Mutex::new(None)),
            shutdown: shutdown_tx,
            stop_started: AtomicBool::new(false),
            stopped: stopped_tx,
            // grace + abort/join 余量：abort 即刻生效，30s 足够
            stop_deadline: spec.stop_grace_timeout + Duration::from_secs(30),
            kind_label: spec.kind.label(),
            bind: bind_label,
            target: spec.target.as_ref().map(|(h, p)| format!("{h}:{p}")),
            session_id: spec.session_id.clone(),
        });

        let group = self.acquire_group(&group_key, connect);
        let task_spec = spec.clone();
        let task_entry = entry.clone();
        let task_group = group.clone();
        let mgr = self.clone();
        self.rt.spawn(async move {
            let result = run_tunnel(
                task_spec,
                task_group.clone(),
                listener,
                task_entry.clone(),
                shutdown_rx,
            )
            .await;
            if let Err(e) = &result {
                if task_entry.stop_started.load(Ordering::Relaxed) {
                    // 用户停止路径中的错误：状态归 Stopped（stop() 负责）
                    tracing::warn!(error = %e, "tunnel error during shutdown");
                } else {
                    // 致命退出：Failed 终态 + 记录错误文本
                    *task_entry.status.lock() = TunnelStatus::Failed;
                    *task_entry.last_error.lock() = Some(e.to_string());
                    tracing::warn!(error = %e, "tunnel task exited with error");
                }
            }
            // 终态信号：stop() 等待者放行（含 Failed 路径——stop 不得悬挂）
            task_entry.stopped.send_replace(true);
            // 对组 lease 归还（最后一个隧道带走组监督器与共享 Transport）
            mgr.release_group(&task_group);
        });

        self.tunnels.lock().insert(id, entry);
        Ok(())
    }

    /// 取/建组（leases+1）：注册表与计数同锁，与 release_group 线性化。
    /// 同组隧道共享一条 Transport——断线由组监督器统一重建一次（PR-15）。
    fn acquire_group(self: &Arc<Self>, key: &str, connect: ConnectFn) -> Arc<TunnelGroup> {
        let mut map = self.groups.lock();
        if let Some(g) = map.get(key) {
            g.leases.fetch_add(1, Ordering::Relaxed);
            return g.clone();
        }
        let (slot_tx, _slot_rx) = watch::channel(Slot::Connecting);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let group = Arc::new(TunnelGroup {
            key: key.to_string(),
            slot: slot_tx.clone(),
            notify: Arc::new(Notify::new()),
            status: Arc::new(Mutex::new(TunnelStatus::Starting)),
            stats: Arc::new(StatsAtomic::default()),
            last_error: Arc::new(Mutex::new(None)),
            leases: AtomicU32::new(1),
            shutdown: shutdown_tx,
            channel_budget: core_policy::Budget::new("tunnel.chan", GROUP_CHANNEL_MAX),
        });
        // 组监督器：原单隧道 supervise_conn 原样提升到组级
        self.rt.spawn(supervise_conn(
            connect,
            slot_tx,
            group.notify.clone(),
            group.status.clone(),
            group.stats.clone(),
            group.last_error.clone(),
            shutdown_rx,
        ));
        map.insert(key.to_string(), group.clone());
        group
    }

    /// 还 lease：归零即关停组监督器并摘出注册表（同锁防 acquire/release 竞态）
    fn release_group(&self, group: &Arc<TunnelGroup>) {
        let mut map = self.groups.lock();
        if group.leases.fetch_sub(1, Ordering::Relaxed) == 1 {
            map.remove(&group.key);
            let _ = group.shutdown.send(true);
        }
    }

    /// 各组 channel 预算快照（PR-17 perf_json governor 节数据源）
    pub fn channel_budgets(&self) -> Vec<core_policy::BudgetSnapshot> {
        self.groups
            .lock()
            .values()
            .map(|g| g.channel_budget.snapshot())
            .collect()
    }

    /// 分阶段停止：标记停 accept → 等 supervisor drain/abort/join 完全部任务 →
    /// 复核 active_conns 归零 → 删 entry。返回时任务与连接已归零；
    /// drain 期间 entry 保留在表内（list/stats 可观测）。
    pub async fn stop(&self, id: &str) -> Result<(), TunnelError> {
        let entry = self.tunnels.lock().get(id).cloned();
        let Some(e) = entry else {
            return Err(TunnelError::NotFound(id.into()));
        };
        // StoppingAccept：幂等闸——并发 stop 等同一完成信号
        if !e.stop_started.swap(true, Ordering::SeqCst) {
            let _ = e.shutdown.send(true);
        }
        // DrainingRelays / ForceAborting 在 run_tunnel 内推进；此处等其 join 全部任务
        let mut rx = e.stopped.subscribe();
        let wait = async {
            while !*rx.borrow_and_update() {
                rx.changed().await.map_err(|_| ())?;
            }
            Ok::<(), ()>(())
        };
        if tokio::time::timeout(e.stop_deadline, wait).await.is_err() {
            // supervisor 异常（panic 级）：fail-loud，仍按流程收尾
            tracing::error!(tunnel = %id, "stop 等待 supervisor 超时（任务可能悬挂）");
        }
        let active = e.stats.active_conns.load(Ordering::Relaxed);
        if active != 0 {
            tracing::error!(tunnel = %id, active, "stop 返回前 active_conns 未归零");
        }
        *e.status.lock() = TunnelStatus::Stopped;
        self.tunnels.lock().remove(id);
        Ok(())
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
                    session_id: e.session_id.clone(),
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
        rejected_conns: s.rejected_conns.load(Ordering::Relaxed),
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
                // 错误分类（PR-4）：认证/HostKey 问题重试无意义，转 Failed 终态提示用户
                if !matches!(e.reconnect_class(), core_ssh::ReconnectClass::Retryable) {
                    *status.lock() = TunnelStatus::Failed;
                    *last_error.lock() = Some(e.to_string());
                    tracing::warn!(error = %e, "tunnel connect failed permanently");
                    return;
                }
                attempt += 1;
                *last_error.lock() = Some(e.to_string());
                tracing::warn!(attempt, error = %e, "tunnel connect failed, backing off");
                // 无限重试（隧道诉求：网络恢复后自动回来）；封顶 30s + equal jitter 防惊群
                let capped = Duration::from_secs((1u64 << attempt.min(5)).min(30));
                let backoff = core_ssh::equal_jitter(capped);
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

/// relay 任务终态（C1：JoinSet 统一收集的返回载体）
#[derive(Debug)]
enum RelayResult {
    /// 正常结束（含半关闭排空）
    Completed,
    /// 建链/协议失败（已计 errors）
    Failed,
}

/// relay 请求：accept/forward 任务组包后经 mpsc 提交
///（C1 方案 A：relay 只能由 supervisor 的 JoinSet spawn，禁止旁路 tokio::spawn）
type RelayReq = std::pin::Pin<Box<dyn Future<Output = RelayResult> + Send>>;

/// 隧道主任务 = relay supervisor（C1 方案 A：独占 JoinSet<RelayResult>）。
/// 结构：spawn 连接监督器 + accept/forward 任务 → 主循环收 relay 请求进 JoinSet →
/// accept 退出（stop/致命错误）后分阶段收尾：drain（grace）→ 强制 abort →
/// join 全部任务 → 断言归零。
async fn run_tunnel(
    spec: TunnelSpec,
    group: Arc<TunnelGroup>,
    listener: Option<std::net::TcpListener>,
    entry: Arc<TunnelEntry>,
    shutdown: watch::Receiver<bool>,
) -> Result<(), TunnelError> {
    let ctx = LinkCtx {
        slot: group.slot.subscribe(),
        notify: group.notify.clone(),
        channel_budget: group.channel_budget.clone(),
        policy: spec.on_disconnect,
        stats: entry.stats.clone(),
        status: entry.status.clone(),
        drain_timeout: spec.half_close_drain_timeout,
    };
    // 组状态镜像 → 隧道状态（连接态由组统一驱动；reconnects 计数归组同步）。
    // Remote 形态的 Listening 由 remote_loop 在 tcpip_forward 注册成功后自行设置，
    // 镜像只推进 Starting/Reconnecting → Listening，不回退。
    let mirror = {
        let mut slot_rx = group.slot.subscribe();
        let gstatus = group.status.clone();
        let gstats = group.stats.clone();
        let gerr = group.last_error.clone();
        let status = entry.status.clone();
        let stats = entry.stats.clone();
        let last_error = entry.last_error.clone();
        let mut shutdown_m = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_m.changed() => break,
                    r = slot_rx.changed() => {
                        if r.is_err() { break; }
                        match *gstatus.lock() {
                            TunnelStatus::Listening => {
                                let mut s = status.lock();
                                if matches!(*s, TunnelStatus::Starting | TunnelStatus::Reconnecting) {
                                    *s = TunnelStatus::Listening;
                                }
                            }
                            TunnelStatus::Reconnecting => *status.lock() = TunnelStatus::Reconnecting,
                            TunnelStatus::Failed => {
                                *status.lock() = TunnelStatus::Failed;
                                *last_error.lock() = gerr.lock().clone();
                            }
                            _ => {}
                        }
                        stats.reconnects.store(
                            gstats.reconnects.load(Ordering::Relaxed),
                            Ordering::Relaxed,
                        );
                    }
                }
            }
        })
    };
    let (req_tx, mut req_rx) = mpsc::channel::<RelayReq>(RELAY_REQ_QUEUE);
    let accept = match spec.kind.clone() {
        TunnelKind::Local { .. } | TunnelKind::DynamicSocks5 { .. } => {
            let Some(listener) = listener else {
                return Err(TunnelError::Listen {
                    bind: entry.bind.clone(),
                    reason: "本地/动态隧道缺监听 socket".into(),
                });
            };
            tokio::spawn(accept_loop(
                spec.clone(),
                listener,
                ctx,
                req_tx,
                shutdown.clone(),
            ))
        }
        TunnelKind::Remote { bind } => tokio::spawn(remote_loop(
            spec.clone(),
            bind,
            ctx,
            req_tx,
            shutdown.clone(),
        )),
    };

    // —— relay 监督主循环：收请求 spawn，收 join_next 汇总 ——
    let mut relays: JoinSet<RelayResult> = JoinSet::new();
    let mut done = 0u64;
    let mut failed = 0u64;
    loop {
        tokio::select! {
            req = req_rx.recv() => match req {
                Some(r) => { relays.spawn(r); }
                // accept/forward 任务退出且队列排空 → 不再新建 relay
                None => break,
            },
            res = relays.join_next(), if !relays.is_empty() => match res {
                Some(Ok(RelayResult::Completed)) => done += 1,
                Some(Ok(RelayResult::Failed)) => failed += 1,
                Some(Err(e)) => {
                    failed += 1;
                    tracing::warn!(?e, "relay task join error");
                }
                None => {}
            },
        }
    }

    // —— 收尾（stop 分阶段：accept 已停 → drain → 超时强制 abort → join 全部任务）——
    let accept_result = match accept.await {
        Ok(r) => r,
        Err(e) => Err(TunnelError::Listen {
            bind: entry.bind.clone(),
            reason: format!("accept 任务 panic: {e}"),
        }),
    };
    // DrainingRelays：已有 relay 按半关闭协议自然排空，上限 stop_grace_timeout
    let drain = async { while relays.join_next().await.is_some() {} };
    if tokio::time::timeout(spec.stop_grace_timeout, drain)
        .await
        .is_err()
    {
        // ForceAborting
        tracing::warn!(
            remaining = relays.len(),
            "stop_grace_timeout 到期，强制 abort 残余 relay"
        );
        relays.abort_all();
        while relays.join_next().await.is_some() {}
    }
    // 组监督器是共享资源，不随单隧道停止——仅收掉状态镜像
    let _ = entry.shutdown.send(true);
    mirror.abort();
    // 断言归零（fail-loud：不归零即任务所有权缺陷）
    let active = entry.stats.active_conns.load(Ordering::Relaxed);
    if active != 0 {
        tracing::error!(active, "tunnel 收尾后 active_conns 未归零");
    }
    tracing::debug!(done, failed, "tunnel relay 汇总");
    accept_result
}

/// 本地/动态隧道的 accept 循环：gating + 组包 relay 请求提交 supervisor
async fn accept_loop(
    spec: TunnelSpec,
    listener: std::net::TcpListener,
    ctx: LinkCtx,
    req_tx: mpsc::Sender<RelayReq>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), TunnelError> {
    let bind_label = format!("{}:{}", spec.kind.bind().0, spec.kind.bind().1);
    let listener = TcpListener::from_std(listener).map_err(|e| TunnelError::Listen {
        bind: bind_label.clone(),
        reason: e.to_string(),
    })?;
    // 并发闸（E4003）：统一 Semaphore，禁止计数器 load+fetch_add 竞态超限。
    // SOCKS5 拆握手/relay 双配额：慢握手客户端不得占满 relay 名额。
    let max_conns = usize::try_from(spec.max_conns)
        .unwrap_or(usize::MAX)
        .min(Semaphore::MAX_PERMITS);
    let relay_sem = Arc::new(Semaphore::new(max_conns));
    let handshake_sem = Arc::new(Semaphore::new(SOCKS_HANDSHAKE_CONCURRENCY));
    let is_socks = matches!(spec.kind, TunnelKind::DynamicSocks5 { .. });

    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accept = listener.accept() => {
                let (tcp, _peer) = match accept {
                    Ok(pair) => pair,
                    Err(e) => {
                        return Err(TunnelError::Listen {
                            bind: bind_label,
                            reason: format!("accept 失败: {e}"),
                        });
                    }
                };
                let _ = tcp.set_nodelay(true);
                let stats = ctx.stats.clone();
                // try_acquire 而非 await：accept 循环不得因等名额停摆，
                // 排队由 OS backlog 承担（显式策略）
                let gate = if is_socks { &handshake_sem } else { &relay_sem };
                let Ok(permit) = gate.clone().try_acquire_owned() else {
                    stats.rejected_conns.fetch_add(1, Ordering::Relaxed);
                    reject_tcp(tcp);
                    continue;
                };
                stats.active_conns.fetch_add(1, Ordering::Relaxed);
                stats.total_conns.fetch_add(1, Ordering::Relaxed);
                let req: RelayReq = match &spec.kind {
                    TunnelKind::DynamicSocks5 { .. } => {
                        Box::pin(handle_socks5(tcp, ctx.clone(), permit, relay_sem.clone()))
                    }
                    _ => match spec.target.clone() {
                        Some((host, port)) => {
                            Box::pin(handle_local(tcp, ctx.clone(), host, port, permit))
                        }
                        None => {
                            // Local 缺 target 是配置错误：拒收计数，不泄漏 active_conns
                            stats.errors.fetch_add(1, Ordering::Relaxed);
                            stats.active_conns.fetch_sub(1, Ordering::Relaxed);
                            continue;
                        }
                    },
                };
                // 队列满则 accept 变慢（有界背压）；supervisor 死亡则退出
                if req_tx.send(req).await.is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// 超限拒收：SO_LINGER=0 立即 RST，客户端快速失败而非挂起
fn reject_tcp(tcp: TcpStream) {
    let _ = socket2::SockRef::from(&tcp).set_linger(Some(Duration::ZERO));
    drop(tcp);
}
/// 建链上下文：开通道重试、统计与半关闭配置所需的共享句柄
#[derive(Clone)]
struct LinkCtx {
    slot: watch::Receiver<Slot>,
    notify: Arc<Notify>,
    /// 组级 Transport channel 上限（PR-15 上限值；PR-17 账本化）
    channel_budget: Arc<core_policy::Budget>,
    policy: DisconnectPolicy,
    stats: Arc<StatsAtomic>,
    /// 远端 tcpip_forward 注册成功后置 Listening（Remote 形态用）
    status: Arc<Mutex<TunnelStatus>>,
    /// 半关闭排空上限（spec.half_close_drain_timeout）
    drain_timeout: Duration,
}

/// _permit 持有 relay 名额至连接结束（Drop 即释放）
async fn handle_local(
    mut tcp: TcpStream,
    ctx: LinkCtx,
    host: String,
    port: u16,
    _permit: OwnedSemaphorePermit,
) -> RelayResult {
    // Transport channel 上限（PR-15）：等待 permit 最多 3s，超时明确拒绝
    let stats = &ctx.stats;
    let _chan_permit = match ctx.channel_budget.acquire_timeout(GROUP_CHANNEL_WAIT).await {
        Ok(p) => p,
        Err(_) => {
            tracing::warn!("Tunnel transport channel limit reached");
            stats.rejected_conns.fetch_add(1, Ordering::Relaxed);
            stats.active_conns.fetch_sub(1, Ordering::Relaxed);
            return RelayResult::Failed;
        }
    };
    // 开通道失败一次 → 通知重连 + 等槽位 → 重试一次；再败计数放弃
    for attempt in 0..2 {
        let Some(conn) = wait_connected(&ctx.slot, ctx.policy).await else {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            stats.active_conns.fetch_sub(1, Ordering::Relaxed);
            return RelayResult::Failed;
        };
        // 建通道超时视同失败：对端不响应不得永久占用任务
        match tokio::time::timeout(CHANNEL_OPEN_TIMEOUT, conn.open_direct_tcpip(&host, port)).await
        {
            Ok(Ok(chan)) => {
                relay(&mut tcp, chan, stats, ctx.drain_timeout).await;
                stats.active_conns.fetch_sub(1, Ordering::Relaxed);
                return RelayResult::Completed;
            }
            Ok(Err(_)) | Err(_) if attempt == 0 => ctx.notify.notify_one(),
            _ => break,
        }
    }
    stats.errors.fetch_add(1, Ordering::Relaxed);
    stats.active_conns.fetch_sub(1, Ordering::Relaxed);
    RelayResult::Failed
}

/// handshake_permit 握手完成即释放；relay 名额在握手成功后才竞争
async fn handle_socks5(
    mut tcp: TcpStream,
    ctx: LinkCtx,
    handshake_permit: OwnedSemaphorePermit,
    relay_sem: Arc<Semaphore>,
) -> RelayResult {
    let stats = &ctx.stats;
    let parsed = tokio::time::timeout(SOCKS_HANDSHAKE_TIMEOUT, socks5_handshake(&mut tcp)).await;
    drop(handshake_permit);
    let target = match parsed {
        Ok(Ok(Some(t))) => t,
        // 协议错误 / IO 错误 / 超时：握手失败计 errors
        Ok(Ok(None)) | Ok(Err(_)) | Err(_) => {
            stats.errors.fetch_add(1, Ordering::Relaxed);
            stats.active_conns.fetch_sub(1, Ordering::Relaxed);
            return RelayResult::Failed;
        }
    };
    let Ok(relay_permit) = relay_sem.try_acquire_owned() else {
        // 满载时回明确 SOCKS5 错误（0x02 connection not allowed），不静默断流
        let _ = tcp
            .write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await;
        stats.rejected_conns.fetch_add(1, Ordering::Relaxed);
        stats.active_conns.fetch_sub(1, Ordering::Relaxed);
        return RelayResult::Failed;
    };
    // reply: 成功（bind 地址填 0.0.0.0:0 占位）
    if tcp
        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .is_err()
    {
        stats.errors.fetch_add(1, Ordering::Relaxed);
        stats.active_conns.fetch_sub(1, Ordering::Relaxed);
        return RelayResult::Failed;
    }
    handle_local(tcp, ctx, target.0, target.1, relay_permit).await
}

/// SOCKS5 无认证 CONNECT 解析：greeting → request；成功/失败 reply 由调用方
/// 在拿到 relay 名额后发送（满载回 0x02 而非默许成功）
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
    Ok(Some((host, port)))
}

/// 远程转发 accept 循环：注册 → 收 forwarded-tcpip 通道 → 组包 relay 请求提交 supervisor
async fn remote_loop(
    spec: TunnelSpec,
    bind: (String, u16),
    ctx: LinkCtx,
    req_tx: mpsc::Sender<RelayReq>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), TunnelError> {
    let Some((target_host, target_port)) = spec.target.clone() else {
        return Err(TunnelError::Listen {
            bind: format!("{}:{}", bind.0, bind.1),
            reason: "远程转发缺目标".into(),
        });
    };
    // 与 Local/Dynamic 同一并发闸（PR-1 补齐：Remote 此前无上限）
    let max_conns = usize::try_from(spec.max_conns)
        .unwrap_or(usize::MAX)
        .min(Semaphore::MAX_PERMITS);
    let relay_sem = Arc::new(Semaphore::new(max_conns));

    'outer: loop {
        // Remote 恒 Queue 策略（行为保持：不受 spec.on_disconnect 影响）
        let Some(conn) = wait_connected(&ctx.slot, DisconnectPolicy::Queue).await else {
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
        *ctx.status.lock() = TunnelStatus::Listening;
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    let _ = conn.cancel_tcpip_forward(&bind.0, bind.1).await;
                    break 'outer;
                }
                ch = rx.recv() => {
                    let Some(ch) = ch else { continue 'outer }; // 路由被移除（重连）→ 重新注册
                    let stats = ctx.stats.clone();
                    // 超限：显式关闭 forwarded channel，独立计数（不归入 errors）
                    let Ok(permit) = relay_sem.clone().try_acquire_owned() else {
                        stats.rejected_conns.fetch_add(1, Ordering::Relaxed);
                        drop(ch);
                        continue;
                    };
                    let (th, tp) = (target_host.clone(), target_port);
                    let drain_timeout = ctx.drain_timeout;
                    let ch_budget = ctx.channel_budget.clone();
                    let req: RelayReq = Box::pin(async move {
                        let _permit = permit; // 名额随 relay 生命周期持有
                        // Transport channel 上限（PR-15）：3s 等待超时明确拒绝
                        let _chan_permit = match ch_budget.acquire_timeout(GROUP_CHANNEL_WAIT).await
                        {
                            Ok(p) => p,
                            Err(_) => {
                                tracing::warn!("Tunnel transport channel limit reached");
                                stats.rejected_conns.fetch_add(1, Ordering::Relaxed);
                                return RelayResult::Failed;
                            }
                        };
                        stats.active_conns.fetch_add(1, Ordering::Relaxed);
                        stats.total_conns.fetch_add(1, Ordering::Relaxed);
                        let result = match tokio::time::timeout(
                            TARGET_CONNECT_TIMEOUT,
                            TcpStream::connect((th.as_str(), tp)),
                        )
                        .await
                        {
                            Ok(Ok(mut tcp)) => {
                                let _ = tcp.set_nodelay(true);
                                relay(&mut tcp, ch.into_stream(), &stats, drain_timeout).await;
                                RelayResult::Completed
                            }
                            Ok(Err(_)) | Err(_) => {
                                stats.errors.fetch_add(1, Ordering::Relaxed);
                                RelayResult::Failed
                            }
                        };
                        stats.active_conns.fetch_sub(1, Ordering::Relaxed);
                        result
                    });
                    if req_tx.send(req).await.is_err() {
                        break 'outer; // supervisor 已死
                    }
                }
            }
        }
    }
    Ok(())
}

/// 半关闭方向终态（PR-7：双向各自维护 completion，不做全局枚举状态机）
enum HalfEnd {
    /// 本端读 EOF：已向对方写侧发 EOF/FIN——另一侧允许排空至自身结束
    Eof,
    /// 写侧失败：本方向停止，另一侧短暂排空
    WriteFailed,
    /// 读重置/协议错误：立即终止双向
    Fatal,
}

/// 双向中继 + 半关闭（PR-7）：有界缓冲（每连接 2×32KB 固定数组），channel window
/// 耗尽时 write 挂起，停止从本地 socket 读取——背压沿链路传导（规格书第 10 条）。
/// 本端 EOF → channel EOF；对端 EOF → TcpStream shutdown(Write)；一侧结束后
/// 另一侧允许排空至 drain_timeout（从一侧 EOF 起算）。
async fn relay(
    tcp: &mut TcpStream,
    chan: russh::ChannelStream<russh::client::Msg>,
    stats: &StatsAtomic,
    drain_timeout: Duration,
) {
    let (mut cr, mut cw) = tokio::io::split(chan);
    let (mut tr, mut tw) = tcp.split();

    // 上行：本地 → SSH。本端 EOF → channel EOF（对端收尾后再由 drop 发 Close）
    let up = async {
        let mut buf = [0u8; RELAY_BUF]; // 固定数组：替代每 relay 32KB Vec 堆分配
        loop {
            match tr.read(&mut buf).await {
                Ok(0) => {
                    let _ = tokio::time::timeout(EOF_SIGNAL_TIMEOUT, cw.shutdown()).await;
                    return HalfEnd::Eof;
                }
                Err(_) => return HalfEnd::Fatal, // 连接重置
                Ok(n) => {
                    if cw.write_all(&buf[..n]).await.is_err() {
                        return HalfEnd::WriteFailed;
                    }
                    stats.bytes_up.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
    };
    // 下行：SSH → 本地。对端 EOF → FIN（尾部数据完整送达后再断）
    let down = async {
        let mut buf = [0u8; RELAY_BUF];
        loop {
            match cr.read(&mut buf).await {
                Ok(0) => {
                    let _ = tokio::time::timeout(EOF_SIGNAL_TIMEOUT, tw.shutdown()).await;
                    return HalfEnd::Eof;
                }
                Err(_) => return HalfEnd::Fatal,
                Ok(n) => {
                    if tw.write_all(&buf[..n]).await.is_err() {
                        return HalfEnd::WriteFailed;
                    }
                    stats.bytes_down.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
    };

    tokio::pin!(up);
    tokio::pin!(down);
    let (ended, up_pending) = tokio::select! {
        e = &mut up => (e, false),
        e = &mut down => (e, true),
    };
    match ended {
        // 连接重置/协议错误：立即终止双向（drop 即关两侧）
        HalfEnd::Fatal => {}
        // EOF/写失败：允许另一侧排空，上限 half_close_drain_timeout
        HalfEnd::Eof | HalfEnd::WriteFailed => {
            let other = async move {
                if up_pending {
                    up.await
                } else {
                    down.await
                }
            };
            let _ = tokio::time::timeout(drain_timeout, other).await;
        }
    }
}
