//! 真实主机 exec 冒烟：ssh_exec <host> <port> <user> <password> <cmd...>
//! 打印 stdout（退出码不收集，exec_collect 只聚合 stdout 到 EOF）。仅诊断/验收用。

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 6 {
        eprintln!("usage: ssh_exec <host> <port> <user> <password> <cmd...>");
        std::process::exit(2);
    }
    let (host, port, user, pass) = (&args[1], &args[2], &args[3], &args[4]);
    let cmd = args[5..].join(" ");
    let opts = ConnectOptions {
        host: host.clone(),
        port: port.parse().unwrap_or_else(|e| panic!("port: {e}")),
        user: user.clone(),
        auth: AuthMethod::Password(zeroize::Zeroizing::new(pass.clone())),
        host_key_check: HostKeyCheck::AcceptAll,
        class: ConnClass::Interactive,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        jump_chain: vec![],
        ki_prompter: None,
    };
    let conn = SshConnection::connect(opts)
        .await
        .unwrap_or_else(|e| panic!("connect: {e}"));
    let out = conn
        .exec_collect(&cmd)
        .await
        .unwrap_or_else(|e| panic!("exec: {e}"));
    print!("{}", String::from_utf8_lossy(&out));
}
