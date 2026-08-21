//! 隧道错误码：E4xxx。

#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    #[error("E4001 监听失败 {bind}: {reason}")]
    Listen { bind: String, reason: String },
    #[error("E4002 隧道不存在: {0}")]
    NotFound(String),
    #[error("E4003 超过连接数上限（{max}）")]
    TooManyConns { max: usize },
    #[error("E4004 超过全局排队字节上限（{max}MB）")]
    QueueBudgetExceeded { max: u64 },
    #[error("E4005 目标不可达 {target}: {reason}")]
    TargetUnreachable { target: String, reason: String },
    #[error("E4006 底层 SSH 错误: {0}")]
    Ssh(#[from] core_ssh::SshError),
}
