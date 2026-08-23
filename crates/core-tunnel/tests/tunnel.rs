//! core-tunnel 集成测试：内存 russh 服务端（direct-tcpip echo + tcpip-forward 回开）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use core_tunnel::{
    ConnectFn, DisconnectPolicy, TunnelKind, TunnelManager, TunnelSpec, TunnelStatus,
};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, MethodKind, MethodSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// direct-tcpip 桥接到真实目标的测试服务端；tcpip_forward 接受并 200ms 后回开通道写标记再读回
struct EchoServer {
    remote_loopback_ok: Arc<AtomicBool>,
}

struct EchoHandler {
    remote_loopback_ok: Arc<AtomicBool>,
}

impl Server for EchoServer {
    type Handler = EchoHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> EchoHandler {
        EchoHandler {
            remote_loopback_ok: self.remote_loopback_ok.clone(),
        }
    }
}

/// 通道 ↔ TCP 双向桥（32KB 双泵，与 spike relay_bridge 同形态）
fn spawn_channel_bridge(ch: Channel<Msg>, mut tcp: TcpStream) {
    tokio::spawn(async move {
        let (mut cr, mut cw) = tokio::io::split(ch.into_stream());
        let (mut tr, mut tw) = tcp.split();
        let up = async move {
            let mut buf = vec![0u8; 32768];
            loop {
                match tr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        };
        let down = async move {
            let mut buf = vec![0u8; 32768];
            loop {
                match cr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        };
        tokio::select! {
            _ = up => {}
            _ = down => {}
        }
    });
}

impl Handler for EchoHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _orig_addr: &str,
        _orig_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        // 真实桥接目标（echo 测试用真实 TCP echo 服务对端）
        let host = host.to_string();
        tokio::spawn(async move {
            match TcpStream::connect((host.as_str(), port as u16)).await {
                Ok(tcp) => {
                    let _ = tcp.set_nodelay(true);
                    reply.accept().await;
                    spawn_channel_bridge(channel, tcp);
                }
                Err(_) => {
                    reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
                }
            }
        });
        Ok(())
    }

    async fn tcpip_forward(
        &mut self,
        _address: &str,
        _port: &mut u32,
        session: &mut Session,
    ) -> Result<bool, Self::Error> {
        // 接受转发，并模拟「服务端 bind 收到入站连接」：回开 forwarded-tcpip 通道
        let handle = session.handle();
        let flag = self.remote_loopback_ok.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let Ok(mut ch) = handle
                .channel_open_forwarded_tcpip("127.0.0.1", 9888, "127.0.0.1", 12345)
                .await
            else {
                return;
            };
            // 写标记 → 期望经客户端隧道 → 本地 echo → 原路弹回
            let marker = b"remote-loopback-marker";
            if ch.data(marker.as_slice()).await.is_err() {
                return;
            }
            let wait = async {
                while let Some(msg) = ch.wait().await {
                    if let russh::ChannelMsg::Data { data } = msg {
                        if data.as_ref() == marker {
                            flag.store(true, Ordering::Relaxed);
                            return;
                        }
                    }
                }
            };
            let _ = tokio::time::timeout(Duration::from_secs(3), wait).await;
        });
        Ok(true)
    }
}

/// 纯 TCP echo 服务（隧道目标端）
async fn start_tcp_echo() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 32768];
                loop {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if s.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    port
}

async fn start_echo_server() -> (u16, Arc<AtomicBool>) {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);
    // 对齐 spike 验证值：默认 event_buffer_size=10 会严重限流（实测 ~35MB/s）
    let config = russh::server::Config {
        methods,
        keys: vec![key],
        window_size: 16 * 1024 * 1024,
        maximum_packet_size: 32768,
        channel_buffer_size: 1024,
        event_buffer_size: 4096,
        nodelay: true,
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let flag = Arc::new(AtomicBool::new(false));
    let srv = EchoServer {
        remote_loopback_ok: flag.clone(),
    };
    tokio::spawn(async move {
        let mut server = srv;
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });
    (port, flag)
}

fn connect_fn(port: u16) -> ConnectFn {
    Arc::new(move || {
        Box::pin(async move {
            SshConnection::connect(ConnectOptions {
                host: "127.0.0.1".into(),
                port,
                user: "tun".into(),
                auth: AuthMethod::None,
                class: ConnClass::Bulk,
                window_size: 4 * 1024 * 1024,
                max_packet_size: 32768,
                keepalive: KeepaliveConfig::default(),
                host_key_check: HostKeyCheck::AcceptAll,
                ki_prompter: None,
            })
            .await
        })
    })
}

fn spec(kind: TunnelKind, target: Option<(String, u16)>) -> TunnelSpec {
    TunnelSpec {
        kind,
        target,
        max_conns: 100,
        on_disconnect: DisconnectPolicy::Queue,
    }
}

async fn wait_listening(mgr: &Arc<TunnelManager>, id: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let listening = mgr
            .list()
            .into_iter()
            .find(|t| t.id == id)
            .map(|i| i.status == TunnelStatus::Listening)
            .unwrap_or(false);
        if listening {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "tunnel not listening");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 本地 -L：隧道口收发 echo 回环
#[tokio::test]
async fn local_forward_echo_roundtrip() {
    let (ssh_port, _) = start_echo_server().await;
    let echo_port = start_tcp_echo().await;
    let mgr = TunnelManager::new();
    mgr.start(
        "lt".into(),
        spec(
            TunnelKind::Local {
                bind: ("127.0.0.1".into(), 0),
            },
            Some(("127.0.0.1".into(), echo_port)),
        ),
        connect_fn(ssh_port),
    )
    .await
    .expect("start tunnel");
    wait_listening(&mgr, "lt").await;

    let bind = mgr.list()[0].bind.clone();
    let mut tcp = TcpStream::connect(&bind).await.expect("connect tunnel");
    tcp.write_all(b"ping-local-tunnel").await.unwrap();
    let mut buf = vec![0u8; 64];
    let n = tcp.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"ping-local-tunnel");

    let st = mgr.stats("lt").expect("stats");
    assert_eq!(st.total_conns, 1);
    assert!(st.bytes_up >= 17 && st.bytes_down >= 17);

    mgr.stop("lt").await.expect("stop");
    assert!(mgr.list().is_empty());
}

/// 动态 -D：SOCKS5 握手后走隧道
#[tokio::test]
async fn dynamic_socks5_roundtrip() {
    let echo_port = start_tcp_echo().await;
    let (ssh_port, _) = start_echo_server().await;
    let mgr = TunnelManager::new();
    mgr.start(
        "dt".into(),
        spec(
            TunnelKind::DynamicSocks5 {
                bind: ("127.0.0.1".into(), 0),
            },
            None,
        ),
        connect_fn(ssh_port),
    )
    .await
    .expect("start");
    wait_listening(&mgr, "dt").await;

    let bind = mgr.list()[0].bind.clone();
    let mut tcp = TcpStream::connect(&bind).await.expect("connect socks");
    // greeting: v5, 1 method, no-auth
    tcp.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let mut g = [0u8; 2];
    tcp.read_exact(&mut g).await.unwrap();
    assert_eq!(g, [0x05, 0x00]);
    // request: CONNECT IPv4 127.0.0.1:echo_port
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&[127, 0, 0, 1]);
    req.extend_from_slice(&echo_port.to_be_bytes());
    tcp.write_all(&req).await.unwrap();
    let mut rep = [0u8; 10];
    tcp.read_exact(&mut rep).await.unwrap();
    assert_eq!(rep[1], 0x00, "socks reply success");

    tcp.write_all(b"ping-socks").await.unwrap();
    let mut buf = vec![0u8; 32];
    let n = tcp.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"ping-socks");

    mgr.stop("dt").await.expect("stop");
}

/// 远程 -R：服务端回开 forwarded-tcpip → 隧道连本地 echo → 标记弹回服务端
#[tokio::test]
async fn remote_forward_loopback() {
    // 本地 echo（隧道目标）
    let echo = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let echo_port = echo.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = echo.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = s.read(&mut buf).await {
                    if n == 0 || s.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    let (ssh_port, flag) = start_echo_server().await;
    let mgr = TunnelManager::new();
    mgr.start(
        "rt".into(),
        spec(
            TunnelKind::Remote {
                bind: ("127.0.0.1".into(), 9888),
            },
            Some(("127.0.0.1".into(), echo_port)),
        ),
        connect_fn(ssh_port),
    )
    .await
    .expect("start");

    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    while !flag.load(Ordering::Relaxed) {
        assert!(
            std::time::Instant::now() < deadline,
            "remote loopback not observed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let st = mgr.stats("rt").expect("stats");
    assert!(st.total_conns >= 1);
    mgr.stop("rt").await.expect("stop");
}

/// 吞吐压测（差分定位 38MB/s 瓶颈）：flood 服务端直连目标，隧道全链 6 秒均值。
/// 软断言 >100MB/s（CI 抗噪）；打印实测值供预算核对（≥400MB/s）。
#[tokio::test]
async fn tunnel_flood_throughput() {
    use tokio::io::AsyncReadExt;

    // flood 目标
    let flood = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let flood_port = flood.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = flood.accept().await {
            tokio::spawn(async move {
                let block = vec![0xABu8; 256 * 1024];
                loop {
                    if s.write_all(&block).await.is_err() {
                        break;
                    }
                }
            });
        }
    });

    // MYSSH_FLOOD_SSH_PORT 指定时连外部服务端（如 dev_shell），否则内存服务端——A/B 差分
    let ssh_port = match std::env::var("MYSSH_FLOOD_SSH_PORT") {
        Ok(p) => p.parse().expect("port"),
        Err(_) => start_echo_server().await.0,
    };
    let mgr = TunnelManager::new();
    mgr.start(
        "flood".into(),
        spec(
            TunnelKind::Local {
                bind: ("127.0.0.1".into(), 0),
            },
            Some(("127.0.0.1".into(), flood_port)),
        ),
        connect_fn(ssh_port),
    )
    .await
    .expect("start");
    wait_listening(&mgr, "flood").await;

    let bind = mgr.list()[0].bind.clone();
    let mut s = TcpStream::connect(&bind).await.unwrap();
    let mut buf = vec![0u8; 256 * 1024];
    let t0 = std::time::Instant::now();
    let mut total = 0u64;
    while t0.elapsed().as_secs() < 5 {
        match s.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => total += n as u64,
            Err(_) => break,
        }
    }
    let rate = total as f64 / 1048576.0 / t0.elapsed().as_secs_f64();
    println!("tunnel flood: {:.1} MB/s", rate);
    assert!(rate > 100.0, "吞吐 {rate:.1} MB/s 远低于 100MB/s 地板");
    mgr.stop("flood").await.expect("stop");
}
