//! 统一错误码：E1xxx 连接 / E2xxx 认证 / E3xxx 通道。
//! Display 内嵌错误码，UI/CLI/审计日志直接引用。

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    // E1xxx 连接
    #[error("E1001 连接失败 {target}: {source}")]
    Connect {
        target: String,
        source: std::io::Error,
    },
    #[error("E1002 连接超时 {target}")]
    ConnectTimeout { target: String },
    #[error("E1003 连接已断开: {reason}")]
    Disconnected { reason: String },
    #[error("E1004 主机密钥校验失败 {host}: {detail}")]
    HostKeyRejected { host: String, detail: String },
    #[error("E1005 主机密钥变更 {host}（需用户确认）")]
    HostKeyChanged { host: String },
    #[error("E1006 known_hosts 读写失败 {host}: {detail}")]
    HostKeyStore { host: String, detail: String },

    // E2xxx 认证
    #[error("E2001 认证失败 {user}@{host}（方法 {method}）")]
    AuthFailed {
        user: String,
        host: String,
        method: String,
    },
    #[error("E2002 凭据不可用: {0}")]
    CredentialUnavailable(String),
    #[error("E2003 不支持的认证方法: {0}")]
    UnsupportedAuth(&'static str),

    // E3xxx 通道
    #[error("E3001 通道打开失败 {kind}: {reason}")]
    ChannelOpen { kind: &'static str, reason: String },
    #[error("E3002 通道被对端拒绝 {kind}: {reason}")]
    ChannelRejected { kind: &'static str, reason: String },
    #[error("E3003 通道 IO 错误: {0}")]
    ChannelIo(String),
    #[error("E3004 内部错误: {0}")]
    Internal(String),
}

/// 重连错误分类（PR-4）：重连策略按稳定枚举分派，禁止匹配错误文本
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconnectClass {
    /// 可恢复网络错误：持续退避重试（隧道默认无限重试）
    Retryable,
    /// 认证失败/凭据不可用/方法不支持：暂停重试并提示用户
    AuthFailed,
    /// 主机密钥校验失败或变更：立即停止（fail-closed）
    HostKeyRejected,
}

impl SshError {
    pub fn reconnect_class(&self) -> ReconnectClass {
        match self {
            Self::AuthFailed { .. } | Self::CredentialUnavailable(_) | Self::UnsupportedAuth(_) => {
                ReconnectClass::AuthFailed
            }
            Self::HostKeyRejected { .. } | Self::HostKeyChanged { .. } => {
                ReconnectClass::HostKeyRejected
            }
            _ => ReconnectClass::Retryable,
        }
    }
}

impl From<russh::Error> for SshError {
    fn from(e: russh::Error) -> Self {
        match e {
            russh::Error::IO(io) => SshError::Connect {
                target: String::new(),
                source: io,
            },
            russh::Error::Disconnect => SshError::Disconnected {
                reason: "peer disconnect".into(),
            },
            russh::Error::ConnectionTimeout => SshError::ConnectTimeout {
                target: String::new(),
            },
            other => SshError::Internal(other.to_string()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_classification() {
        let auth = SshError::AuthFailed {
            user: "u".into(),
            host: "h".into(),
            method: "password".into(),
        };
        assert_eq!(auth.reconnect_class(), ReconnectClass::AuthFailed);
        assert_eq!(
            SshError::CredentialUnavailable("x".into()).reconnect_class(),
            ReconnectClass::AuthFailed
        );
        assert_eq!(
            SshError::UnsupportedAuth("x").reconnect_class(),
            ReconnectClass::AuthFailed
        );
        assert_eq!(
            SshError::HostKeyChanged { host: "h".into() }.reconnect_class(),
            ReconnectClass::HostKeyRejected
        );
        assert_eq!(
            SshError::ConnectTimeout { target: "h".into() }.reconnect_class(),
            ReconnectClass::Retryable
        );
        assert_eq!(
            SshError::Disconnected { reason: "r".into() }.reconnect_class(),
            ReconnectClass::Retryable
        );
    }
}
