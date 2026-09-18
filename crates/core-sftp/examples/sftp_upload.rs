//! 真实主机 SFTP 上传冒烟：sftp_upload <host> <port> <user> <password> <local> <remote>
//! 仅诊断/验收用（trzsz 真机验收把二进制推上无外网的服务器）。

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 7 {
        eprintln!("usage: sftp_upload <host> <port> <user> <password> <local> <remote>");
        std::process::exit(2);
    }
    let (host, port, user, pass) = (&args[1], &args[2], &args[3], &args[4]);
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
    let sftp = core_sftp::SftpClient::open(&conn)
        .await
        .unwrap_or_else(|e| panic!("sftp: {e}"));
    let data = std::fs::read(&args[5]).unwrap_or_else(|e| panic!("读本地文件: {e}"));
    sftp.overwrite(&args[6], &data)
        .await
        .unwrap_or_else(|e| panic!("上传: {e}"));
    println!("uploaded {} bytes -> {}", data.len(), args[6]);
}
