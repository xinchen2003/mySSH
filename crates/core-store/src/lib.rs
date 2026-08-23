//! core-store：SQLite 会话元数据 + 凭据保险库 + append-only 审计。
//!
//! 设计契约见 docs/design/02-core-api.md。
//! - 元数据：sqlx/SQLite（sessions、tunnels、audit 表），打开即自动 migrate
//! - 凭据：Windows DPAPI(CurrentUser) 托管（跨平台主密码档预留 unlock 接口）；
//!   `Secret` 不 Serialize（安全模型第 4 条）
//! - 审计：append-only，无 UPDATE/DELETE 接口（安全模型第 5 条）
//!
//! 错误码段：E7xxx。

mod audit;
mod cred;
mod error;
pub mod export;
pub mod import;
pub mod import_ext;
mod session;
mod tunnel;

use std::path::Path;

use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;

pub use audit::{Actor, AuditRecord, AuditRepo};
pub use cred::{CredentialKind, CredentialStore, VaultStatus};
pub use error::StoreError;
pub use export::{export_encrypted, export_plain, import_config, ConfigImportOutcome};
pub use import::{import_openssh, ImportOutcome};
pub use session::{AuthType, SessionRecord, SessionRepo};
pub use tunnel::{TunnelRecord, TunnelRepo};

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

pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// 打开（不存在则创建）并自动 migrate。WAL 模式兼顾并发读写。
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Open {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| StoreError::Open {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;
        sqlx::migrate!("./migrations")
            .run(&pool)
            .await
            .map_err(|e| StoreError::Migration(e.to_string()))?;
        Ok(Self { pool })
    }

    pub fn sessions(&self) -> SessionRepo {
        SessionRepo::new(self.pool.clone())
    }

    pub fn credentials(&self) -> CredentialStore {
        CredentialStore::new(self.pool.clone())
    }

    pub fn audit(&self) -> AuditRepo {
        AuditRepo::new(self.pool.clone())
    }

    pub fn tunnels(&self) -> TunnelRepo {
        TunnelRepo::new(self.pool.clone())
    }
}
