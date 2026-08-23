//! 独立隧道基准：脱离 Tauri 进程，直连 dev_shell/spike-server，测 core-tunnel 纯客户端栈。
//!
//!   flood_bench <ssh_port> <flood_target_port> <secs>
//!   例：dev_shell(2323) + flood_target(9998) → flood_bench 2323 9998 6

use std::sync::Arc;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use core_tunnel::{DisconnectPolicy, TunnelKind, TunnelManager, TunnelSpec};
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let ssh_port: u16 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(2323);
    let target_port: u16 = std::env::args()
        .nth(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(9998);
    let secs: u64 = std::env::args()
        .nth(3)
        .and_then(|a| a.parse().ok())
        .unwrap_or(6);

    let connect: core_tunnel::ConnectFn = Arc::new(move || {
        Box::pin(async move {
            SshConnection::connect(ConnectOptions {
                host: "127.0.0.1".into(),
                port: ssh_port,
                user: "bench".into(),
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

    let mgr = TunnelManager::new();
    mgr.start(
        "bench".into(),
        TunnelSpec {
            kind: TunnelKind::Local {
                bind: ("127.0.0.1".into(), 0),
            },
            target: Some(("127.0.0.1".into(), target_port)),
            max_conns: 100,
            on_disconnect: DisconnectPolicy::Queue,
        },
        connect,
    )
    .await
    .unwrap_or_else(|e| panic!("start tunnel: {e}"));

    // 等监听
    let bind = loop {
        let ready = mgr
            .list()
            .into_iter()
            .find(|t| t.id == "bench")
            .map(|i| i.status == core_tunnel::TunnelStatus::Listening)
            .unwrap_or(false);
        if ready {
            break mgr.list()[0].bind.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    println!("tunnel listening on {bind}");

    let mut s = TcpStream::connect(&bind)
        .await
        .unwrap_or_else(|e| panic!("connect tunnel: {e}"));
    let mut buf = vec![0u8; 256 * 1024];
    let t0 = std::time::Instant::now();
    let mut total = 0u64;
    while t0.elapsed().as_secs() < secs {
        match s.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => total += n as u64,
            Err(_) => break,
        }
    }
    let secs_f = t0.elapsed().as_secs_f64();
    println!(
        "bench: {:.1} MB/s ({:.1} MB in {:.2}s)",
        total as f64 / 1048576.0 / secs_f,
        total as f64 / 1048576.0,
        secs_f
    );
}
