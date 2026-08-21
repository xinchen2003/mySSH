//! 策略错误码：E6xxx。

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("E6001 权限拒绝：{detail}")]
    Denied { detail: String },
    #[error("E6002 触发速率限制：{limit}/分钟")]
    RateLimited { limit: u32 },
    #[error("E6003 AI 访问已被全局切断")]
    KillSwitchActive,
}
