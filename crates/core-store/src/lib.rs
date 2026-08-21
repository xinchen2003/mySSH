//! core-store：SQLite 会话元数据 + 凭据保险库 + append-only 审计。
//!
//! 设计契约见 docs/design/02-core-api.md。
//! - 元数据：sqlx/SQLite（sessions、groups、tunnels、audit 表）
//! - 凭据：Argon2id 主密码派生 或 Windows DPAPI 委托；`Secret` 不 Serialize（安全模型第 4 条）
//! - 审计：append-only，无 UPDATE/DELETE 接口（安全模型第 5 条）
//!
//! 错误码段：E7xxx。M0 为空壳。

mod error;

pub use error::StoreError;

/// 凭据载体：不实现 Serialize/Clone 的 Debug 明文输出；Drop 时零化。
#[derive(zeroize::ZeroizeOnDrop)]
pub struct Secret(Vec<u8>);

impl Secret {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// 仅在内存中使用（如 russh 认证入参）；调用方不得持久化返回值
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
