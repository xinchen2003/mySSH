//! 凭据保险库：credentials 表 + Windows DPAPI(CurrentUser) 加解密。
//!
//! 安全模型第 4 条：秘密材料只以密文落盘；`Secret` 不 Serialize、Drop 零化。
//! 非 Windows 平台暂无密钥托管（Argon2id 主密码档留待跨平台需求），运行时返回 Unsupported。

use sqlx::SqlitePool;

use crate::error::StoreError;
use crate::Secret;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialKind {
    Password,
    KeyPassphrase,
    /// 登录后切换用户（su）的密码（批次二十二）
    SuPassword,
}

impl CredentialKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::KeyPassphrase => "key_passphrase",
            Self::SuPassword => "su_password",
        }
    }

    pub fn parse(s: &str) -> Result<Self, StoreError> {
        match s {
            "password" => Ok(Self::Password),
            "key_passphrase" => Ok(Self::KeyPassphrase),
            "su_password" => Ok(Self::SuPassword),
            other => Err(StoreError::Corrupt(format!("未知凭据类型 {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultStatus {
    /// DPAPI 托管：无"锁定"概念，系统级保护随用户会话生效
    Unlocked,
    /// 预留：主密码档锁定态
    Locked,
}

pub struct CredentialStore {
    pool: SqlitePool,
}

impl CredentialStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub fn status(&self) -> VaultStatus {
        VaultStatus::Unlocked
    }

    /// DPAPI 档无需主密码；保留接口对齐 02 契约（跨平台主密码档预留）
    pub async fn unlock(&self, _master: Option<&str>) -> Result<(), StoreError> {
        Ok(())
    }

    pub async fn put(
        &self,
        session_id: &str,
        kind: CredentialKind,
        secret: &Secret,
    ) -> Result<(), StoreError> {
        let blob = protect(secret.expose())?;
        sqlx::query(
            "INSERT INTO credentials (session_id, kind, blob, updated_at)
             VALUES (?,?,?,datetime('now'))
             ON CONFLICT(session_id, kind) DO UPDATE SET blob=excluded.blob, updated_at=excluded.updated_at",
        )
        .bind(session_id)
        .bind(kind.as_str())
        .bind(blob)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Query(e.to_string()))?;
        Ok(())
    }

    /// 取指定类型凭据（迁移 0009 起每会话可并存多条，必须带 kind）
    pub async fn get(&self, session_id: &str, kind: CredentialKind) -> Result<Secret, StoreError> {
        let row: Option<(Vec<u8>,)> =
            sqlx::query_as("SELECT blob FROM credentials WHERE session_id = ? AND kind = ?")
                .bind(session_id)
                .bind(kind.as_str())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| StoreError::Query(e.to_string()))?;
        let (blob,) =
            row.ok_or_else(|| StoreError::NotFound(format!("credential {session_id}")))?;
        Ok(Secret::new(unprotect(&blob)?))
    }

    /// 取会话全部凭据（配置导出用）
    pub async fn get_all(
        &self,
        session_id: &str,
    ) -> Result<Vec<(CredentialKind, Secret)>, StoreError> {
        let rows: Vec<(String, Vec<u8>)> =
            sqlx::query_as("SELECT kind, blob FROM credentials WHERE session_id = ?")
                .bind(session_id)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| StoreError::Query(e.to_string()))?;
        rows.into_iter()
            .map(|(kind, blob)| {
                Ok((
                    CredentialKind::parse(&kind)?,
                    Secret::new(unprotect(&blob)?),
                ))
            })
            .collect()
    }

    /// 删除指定类型；kind=None 删会话全部凭据（会话删除级联用）
    pub async fn delete(
        &self,
        session_id: &str,
        kind: Option<CredentialKind>,
    ) -> Result<(), StoreError> {
        match kind {
            Some(k) => sqlx::query("DELETE FROM credentials WHERE session_id = ? AND kind = ?")
                .bind(session_id)
                .bind(k.as_str())
                .execute(&self.pool)
                .await
                .map_err(|e| StoreError::Query(e.to_string()))?,
            None => sqlx::query("DELETE FROM credentials WHERE session_id = ?")
                .bind(session_id)
                .execute(&self.pool)
                .await
                .map_err(|e| StoreError::Query(e.to_string()))?,
        };
        Ok(())
    }
}

#[cfg(windows)]
fn protect(plain: &[u8]) -> Result<Vec<u8>, StoreError> {
    windows_dpapi::encrypt_data(plain, windows_dpapi::Scope::User, None)
        .map_err(|e| StoreError::Crypto(e.to_string()))
}

#[cfg(windows)]
fn unprotect(blob: &[u8]) -> Result<Vec<u8>, StoreError> {
    windows_dpapi::decrypt_data(blob, windows_dpapi::Scope::User, None)
        .map_err(|e| StoreError::Crypto(e.to_string()))
}

#[cfg(not(windows))]
fn protect(_plain: &[u8]) -> Result<Vec<u8>, StoreError> {
    Err(StoreError::Crypto(
        "当前平台暂无密钥托管（需 DPAPI 或主密码档）".into(),
    ))
}

#[cfg(not(windows))]
fn unprotect(_blob: &[u8]) -> Result<Vec<u8>, StoreError> {
    Err(StoreError::Crypto(
        "当前平台暂无密钥托管（需 DPAPI 或主密码档）".into(),
    ))
}
