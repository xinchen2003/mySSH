//! SFTP 错误码：E5xxx。

#[derive(Debug, thiserror::Error)]
pub enum SftpError {
    #[error("E5001 子系统打开失败: {0}")]
    Subsystem(String),
    #[error("E5002 远程路径错误 {path}: {reason}")]
    RemotePath { path: String, reason: String },
    #[error("E5003 本地 IO 错误 {path}: {reason}")]
    LocalIo { path: String, reason: String },
    #[error("E5004 传输中断（已完成 {done}/{total} 字节）")]
    Interrupted { done: u64, total: u64 },
    #[error("E5005 底层 SSH 错误: {0}")]
    Ssh(#[from] core_ssh::SshError),
}
