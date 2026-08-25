//! 存储错误码：E7xxx。

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("E7001 数据库打开失败 {path}: {reason}")]
    Open { path: String, reason: String },
    #[error("E7002 迁移失败: {0}")]
    Migration(String),
    #[error("E7003 保险库已锁定")]
    VaultLocked,
    #[error("E7004 主密码错误")]
    BadMasterPassword,
    #[error("E7005 加密/解密失败: {0}")]
    Crypto(String),
    #[error("E7006 记录不存在: {0}")]
    NotFound(String),
    #[error("E7007 数据库操作失败: {0}")]
    Query(String),
    #[error("E7008 数据损坏: {0}")]
    Corrupt(String),
    #[error("E7009 无效输入: {0}")]
    Validation(String),
}
