//! PR-18 性能门禁（PR 级微基准）：隧道 relay 回环。
//!
//! 全自包含：进程内 russh echo 服务端 + TCP echo 目标 + 本地 -L 隧道，
//! 逐连接收发 1MiB，输出 median 延迟与聚合吞吐，超阈值退出码非零。
//!
//!   cargo run --release -p core-tunnel --example gate_relay
//!
//! 阈值是本机首测 median × 约 2 倍裕度（回归探测器，非绝对性能承诺）；
//! 机器忙时数值上移属预期——阈值只在相对同机历史退化时命中。

// 门禁 example：基准程序崩溃即失败，expect 是诚实的失败路径
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use core_tunnel::{DisconnectPolicy, TunnelKind, TunnelManager, TunnelSpec};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, MethodKind, MethodSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const CONNS: usize = 32;
const PAYLOAD: usize = 1024 * 1024;
/// 阈值（本机 release 首测 median ~8ms/conn、~950MiB/s；2 倍裕度取整）
const MEDIAN_CONN_MS_MAX: f64 = 50.0;
const THROUGHPUT_MIBS_MIN: f64 = 300.0;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

// ---- 进程内 SSH echo 服务端（复刻 tests/tunnel.rs 的 harness）----

struct GateServer;

struct GateHandler;

impl Server for GateServer {
    type Handler = GateHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> GateHandler {
        GateHandler
    }
}

fn spawn_channel_bridge(ch: Channel<Msg>, tcp: TcpStream) {
    tokio::spawn(async move {
        let (mut cr, mut cw) = tokio::io::split(ch.into_stream());
        let (mut tr, mut tw) = tokio::io::split(tcp);
        let up = async move {
            let mut buf = [0u8; 32768];
            loop {
                match tr.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        let _ = cw.shutdown().await;
                        break;
                    }
                    Ok(n) => {
                        if cw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        };
        let down = async move {
            let mut buf = [0u8; 32768];
            loop {
                match cr.read(&mut buf).await {
                    Ok(0) | Err(_) => {
                        let _ = tw.shutdown().await;
                        break;
                    }
                    Ok(n) => {
                        if tw.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                }
            }
        };
        let _ = tokio::join!(up, down);
    });
}

impl Handler for GateHandler {
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
}

async fn start_ssh_echo() -> u16 {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).expect("host key");
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);
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
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        let _ = GateServer.run_on_socket(Arc::new(config), &listener).await;
    });
    port
}

async fn start_tcp_echo() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
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

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let ssh_port = start_ssh_echo().await;
    let echo_port = start_tcp_echo().await;

    let mgr = TunnelManager::new();
    let connect: core_tunnel::ConnectFn = Arc::new(move || {
        Box::pin(async move {
            SshConnection::connect(ConnectOptions {
                host: "127.0.0.1".into(),
                port: ssh_port,
                user: "gate".into(),
                auth: AuthMethod::None,
                class: ConnClass::Bulk,
                window_size: 16 * 1024 * 1024,
                max_packet_size: 32768,
                keepalive: KeepaliveConfig::default(),
                jump_chain: vec![],
                host_key_check: HostKeyCheck::AcceptAll,
                ki_prompter: None,
            })
            .await
        })
    });
    mgr.start(
        "gate".into(),
        TunnelSpec {
            kind: TunnelKind::Local {
                bind: ("127.0.0.1".into(), 0),
            },
            target: Some(("127.0.0.1".into(), echo_port)),
            max_conns: 1024,
            on_disconnect: DisconnectPolicy::Queue,
            stop_grace_timeout: Duration::from_millis(500),
            half_close_drain_timeout: Duration::from_secs(5),
        },
        "gate".into(),
        connect,
    )
    .await
    .expect("start tunnel");
    let bind = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(t) = mgr
                .list()
                .into_iter()
                .find(|t| t.status == core_tunnel::TunnelStatus::Listening)
            {
                break t.bind;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("listening");

    let payload = vec![0xabu8; PAYLOAD];
    let mut lat_ms = Vec::with_capacity(CONNS);
    let wall = Instant::now();
    for _ in 0..CONNS {
        let t0 = Instant::now();
        let mut tcp = TcpStream::connect(&bind).await.expect("connect");
        tcp.write_all(&payload).await.expect("write");
        tcp.shutdown().await.expect("shutdown write");
        let mut got = Vec::with_capacity(PAYLOAD);
        tcp.read_to_end(&mut got).await.expect("read");
        assert_eq!(got.len(), PAYLOAD, "echo 必须全量回环");
        lat_ms.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let wall_s = wall.elapsed().as_secs_f64();
    let med = median(lat_ms);
    let mibs = (CONNS * PAYLOAD) as f64 / 1024.0 / 1024.0 / wall_s;
    let _ = mgr.stop("gate").await;

    let ok_med = med <= MEDIAN_CONN_MS_MAX;
    let ok_tput = mibs >= THROUGHPUT_MIBS_MIN;
    println!("GATE relay.medianConnMs value={med:.1} threshold={MEDIAN_CONN_MS_MAX} ok={ok_med}");
    println!(
        "GATE relay.throughputMiBs value={mibs:.0} threshold={THROUGHPUT_MIBS_MIN} ok={ok_tput}"
    );
    if !(ok_med && ok_tput) {
        eprintln!("gate_relay 未过门禁");
        std::process::exit(1);
    }
}
