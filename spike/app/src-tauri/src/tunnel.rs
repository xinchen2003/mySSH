//! 隧道：独立线程 + 独立 tokio runtime（规格书第 8 条 runtime 隔离）。
//! 数据路径零 IPC：本地 TcpStream ↔ SSH direct-tcpip channel 直接中继（第 9 条）。

use std::sync::atomic::Ordering;
use std::time::Instant;

use anyhow::Result;
use russh::client::Msg;
use russh::ChannelStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::ssh::{self, ClientHandle};
use crate::stats;

/// 中继缓冲：对齐 channel max_packet_size
const RELAY_BUF: usize = 32 * 1024;

pub enum TunnelCmd {
    Start {
        listen_port: u16,
        target_port: u16,
        result: oneshot::Sender<Result<(), String>>,
    },
    StopAll,
}

#[derive(Clone)]
pub struct TunnelHandle {
    tx: mpsc::UnboundedSender<TunnelCmd>,
}

impl TunnelHandle {
    pub fn start(&self, listen_port: u16, target_port: u16) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let _ = self.tx.send(TunnelCmd::Start { listen_port, target_port, result: tx });
        rx
    }

    pub fn stop_all(&self) {
        let _ = self.tx.send(TunnelCmd::StopAll);
    }
}

/// 独立线程上跑独立 runtime，隧道负载不占用交互链路的调度时间片
pub fn spawn_tunnel_thread() -> TunnelHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("tunnel-rt".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .thread_name("tunnel-worker")
                .build()
                .expect("tunnel runtime");
            rt.block_on(run(rx));
        })
        .expect("spawn tunnel thread");
    TunnelHandle { tx }
}

/// 大 backlog 监听：默认 SOMAXCONN 在 500 并发 SYN 突发下溢出 → SYN 重传 1s，
/// 建连 P99 被拉高（实测踩中）。隧道入口必须显式放大 backlog。
fn bind_listener(port: u16) -> std::io::Result<TcpListener> {
    use socket2::{Domain, SockAddr, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None)?;
    socket.set_nonblocking(true)?;
    socket.set_reuse_address(true)?;
    socket.bind(&SockAddr::from(std::net::SocketAddr::from(([127, 0, 0, 1], port))))?;
    socket.listen(4096)?;
    TcpListener::from_std(socket.into())
}

async fn run(mut rx: mpsc::UnboundedReceiver<TunnelCmd>) {
    let mut client: Option<ClientHandle> = None;
    let mut listeners: Vec<tokio::task::AbortHandle> = Vec::new();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            TunnelCmd::Start { listen_port, target_port, result } => {
                let out = async {
                    if client.is_none() {
                        // 隧道使用独立 SSH 连接（独立 TCP+握手），规格书第 6 条
                        client = Some(ssh::connect().await.map_err(|e| e.to_string())?);
                        info!("tunnel ssh connection established");
                    }
                    let listener = bind_listener(listen_port).map_err(|e| e.to_string())?;
                    let handle = client.clone().expect("client just connected");
                    let task = tokio::spawn(accept_loop(listener, handle, target_port));
                    listeners.push(task.abort_handle());
                    info!(listen_port, target_port, "tunnel listening");
                    Ok::<(), String>(())
                }
                .await;
                let _ = result.send(out);
            }
            TunnelCmd::StopAll => {
                for l in listeners.drain(..) {
                    l.abort();
                }
                info!("all tunnel listeners stopped");
            }
        }
    }
}

async fn accept_loop(listener: TcpListener, client: ClientHandle, target_port: u16) {
    loop {
        match listener.accept().await {
            Ok((tcp, peer)) => {
                let _ = tcp.set_nodelay(true);
                let client = client.clone();
                tokio::spawn(async move {
                    let t0 = Instant::now();
                    match client
                        .channel_open_direct_tcpip("127.0.0.1", target_port as u32, "127.0.0.1", 0u32)
                        .await
                    {
                        Ok(channel) => {
                            stats::record_connect_lat(t0.elapsed().as_micros() as u64);
                            stats::ACTIVE_CONNS.fetch_add(1, Ordering::Relaxed);
                            stats::TOTAL_CONNS.fetch_add(1, Ordering::Relaxed);
                            relay(tcp, channel.into_stream()).await;
                            stats::ACTIVE_CONNS.fetch_sub(1, Ordering::Relaxed);
                        }
                        Err(e) => {
                            stats::CONNECT_ERRORS.fetch_add(1, Ordering::Relaxed);
                            warn!(?e, %peer, "direct-tcpip open failed");
                        }
                    }
                });
            }
            Err(e) => {
                warn!(?e, "accept failed");
                break;
            }
        }
    }
}

/// 双向中继：有界缓冲（每连接 2×32KB），channel window 耗尽时 write_all 自然挂起，
/// 停止从本地 socket 读取 —— 背压沿链路传导，不在内存堆积（规格书第 10 条）。
async fn relay(tcp: TcpStream, chan: ChannelStream<Msg>) {
    let (mut tr, mut tw) = tokio::io::split(tcp);
    let (mut cr, mut cw) = tokio::io::split(chan);

    let up = async move {
        let mut buf = vec![0u8; RELAY_BUF];
        loop {
            match tr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if cw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                    stats::UP_BYTES.fetch_add(n as u64, Ordering::Relaxed);
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
                    stats::DOWN_BYTES.fetch_add(n as u64, Ordering::Relaxed);
                }
            }
        }
    };

    // 任一方向结束即收尾（sink/source 场景另一端随之无意义）
    tokio::select! {
        _ = up => {}
        _ = down => {}
    }
}
