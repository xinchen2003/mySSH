//! 真机 sshd 的 SFTP 上传/下载回环 E2E（轴一 1.3）：CI OpenSSH 容器内跑。
//! 环境变量同 core-ssh e2e（MYSSH_E2E_HOST 缺省即跳过）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use zeroize::Zeroizing;

fn e2e_opts() -> Option<ConnectOptions> {
    let host = std::env::var("MYSSH_E2E_HOST").ok()?;
    Some(ConnectOptions {
        host,
        port: std::env::var("MYSSH_E2E_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22),
        user: std::env::var("MYSSH_E2E_USER").unwrap_or_else(|_| "tester".into()),
        auth: AuthMethod::Password(Zeroizing::new(
            std::env::var("MYSSH_E2E_PASSWORD").expect("MYSSH_E2E_PASSWORD 未设置"),
        )),
        jump_chain: vec![],
        class: ConnClass::Interactive,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    })
}

#[tokio::test]
async fn upload_download_roundtrip_byte_identical() {
    let Some(opts) = e2e_opts() else { return };
    let conn = SshConnection::connect(opts).await.expect("连接失败");
    let sftp = core_sftp::SftpClient::open(&conn)
        .await
        .expect("开 SFTP 失败");

    // 确定性伪随机内容（256KiB，跨 SFTP 分块边界）
    let data: Vec<u8> = (0..256 * 1024u32).map(|i| (i * 31 % 251) as u8).collect();
    let remote = format!("/tmp/myssh-e2e-{}.bin", std::process::id());

    sftp.overwrite(&remote, &data).await.expect("上传失败");

    use tokio::io::AsyncReadExt;
    let mut f = sftp.open_read(&remote).await.expect("打开远端文件失败");
    let mut got = Vec::new();
    f.read_to_end(&mut got).await.expect("下载失败");
    assert_eq!(got.len(), data.len(), "回环长度不一致");
    assert_eq!(got, data, "回环内容不一致");

    sftp.remove_file(&remote).await.expect("清理失败");
}
