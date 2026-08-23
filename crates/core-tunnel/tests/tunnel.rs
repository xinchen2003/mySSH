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
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// direct-tcpip echo 服务端；tcpip_forward 接受并 200ms 后回开一条通道写标记再读回
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

/// 在服务端通道上跑 echo（客户端 channel 发来的数据原样写回）
fn spawn_channel_echo<T: From<(ChannelId, russh::ChannelMsg)> + Send + Sync + 'static>(
    mut ch: Channel<T>,
) {
    tokio::spawn(async move {
        while let Some(msg) = ch.wait().await {
            if let russh::ChannelMsg::Data { data } = msg {
                // russh 0.62 Channel::data 取 AsyncRead：切片即 reader
                if ch.data(&data[..]).await.is_err() {
                    break;
                }
            }
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
        _host: &str,
        _port: u32,
        _orig_addr: &str,
        _orig_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        spawn_channel_echo(channel);
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

async fn start_echo_server() -> (u16, Arc<AtomicBool>) {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);
    let config = russh::server::Config {
        methods,
        keys: vec![key],
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
    let mgr = TunnelManager::new();
    mgr.start(
        "lt".into(),
        spec(
            TunnelKind::Local {
                bind: ("127.0.0.1".into(), 0),
            },
            Some(("127.0.0.1".into(), 9999)),
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
    // request: CONNECT IPv4 127.0.0.1:9999
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&[127, 0, 0, 1]);
    req.extend_from_slice(&9999u16.to_be_bytes());
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
