//! 采集器集成测试：内存 SSH 服务端按轮次返回 /proc 夹具，验证差分与降级。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use core_monitor::MetricsCollector;
use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::net::TcpListener;

/// 两轮 /proc 内容：CPU 总 jiffies +1000、idle +500 → 忙率 50%；eth0 rx +2048B。
fn round_content(round: usize) -> String {
    let (cpu, rx) = if round == 0 {
        (("4705 356 584 16205 229 0 23 0"), "1000")
    } else {
        (("5205 356 584 16705 229 0 23 0"), "3048")
    };
    format!(
        "==STAT==\ncpu  {cpu} 0 0\n==MEM==\nMemTotal: 3867720 kB\nMemAvailable: 2100400 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n==LOAD==\n0.50 0.40 0.30 2/345 999\n==DISK==\n   8       0 sda 1000 0 204800 500 2000 0 409600 800 0 0 0\n==NET==\nInter-|h\n face |h\n  eth0: {rx} 1 0 0 0 0 0 0 2000 1 0 0 0 0 0 0\n==PS==\n 1234 102400 12.5 2.6 java\n"
    )
}

struct TestServer {
    rounds: Arc<AtomicUsize>,
    noproc: bool,
}
struct TestHandler {
    rounds: Arc<AtomicUsize>,
    noproc: bool,
}

impl Server for TestServer {
    type Handler = TestHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> TestHandler {
        TestHandler {
            rounds: self.rounds.clone(),
            noproc: self.noproc,
        }
    }
}

impl Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let body = if self.noproc {
            "sh: cat: not found".to_string()
        } else {
            round_content(self.rounds.fetch_add(1, Ordering::SeqCst))
        };
        let handle = session.handle();
        tokio::spawn(async move {
            let _ = handle.data(channel, body).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
}

async fn start_server(noproc: bool) -> (u16, Arc<AtomicUsize>) {
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
    let rounds = Arc::new(AtomicUsize::new(0));
    let rounds2 = rounds.clone();
    tokio::spawn(async move {
        let mut server = TestServer {
            rounds: rounds2,
            noproc,
        };
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });
    (port, rounds)
}

async fn connect(port: u16) -> SshConnection {
    SshConnection::connect(ConnectOptions {
        host: "127.0.0.1".into(),
        port,
        user: "test".into(),
        auth: AuthMethod::None,
        class: ConnClass::Bulk,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        jump_chain: vec![],
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    })
    .await
    .expect("connect")
}

#[tokio::test]
async fn two_rounds_produce_deltas() {
    let (port, _rounds) = start_server(false).await;
    let conn = connect(port).await;
    let mut c = MetricsCollector::new();

    let s1 = c.collect(&conn).await.expect("round1");
    assert_eq!(s1.interval_ms, 0);
    assert!(s1.cpu_busy_pct.is_none(), "首轮无差分");
    assert!(s1.nets[0].rx_bps.is_none());
    assert_eq!(s1.mem_total_kb, 3867720);
    assert_eq!(s1.load, [0.5, 0.4, 0.3]);
    assert_eq!(s1.procs[0].comm, "java");
    assert_eq!(s1.disks.len(), 1);

    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    let s2 = c.collect(&conn).await.expect("round2");
    assert!(s2.interval_ms >= 100);
    let busy = s2.cpu_busy_pct.expect("次轮有差分");
    // (1000 总 jiffies - 500 idle) / 1000 = 50%
    assert!((busy - 50.0).abs() < 1.0, "busy={busy}");
    // rx 增量 2048B / dt ≈ 17KB/s 量级（dt≈0.12s）
    let rx = s2.nets[0].rx_bps.expect("rx rate");
    assert!(rx > 10_000 && rx < 1_000_000, "rx={rx}");
    let dr = s2.disks[0].read_bps.expect("disk rate");
    assert_eq!(dr, 0, "两轮 diskstats 相同 → 速率为 0");
}

#[tokio::test]
async fn noprocfs_degrades_to_error() {
    let (port, _rounds) = start_server(true).await;
    let conn = connect(port).await;
    let mut c = MetricsCollector::new();
    let err = match c.collect(&conn).await {
        Ok(_) => panic!("无 /proc 输出必须报 NoProcfs"),
        Err(e) => e,
    };
    assert!(
        matches!(err, core_monitor::MonitorError::NoProcfs),
        "err={err}"
    );
}
