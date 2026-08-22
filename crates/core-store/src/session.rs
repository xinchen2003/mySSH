//! 会话档案仓储：sessions 表 CRUD（无秘密材料）。

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::error::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthType {
    Password,
    /// DB/CLI 一律 "publickey"（与 OpenSSH 术语一致，不做 kebab 拆词）
    #[serde(rename = "publickey")]
    PublicKey,
    KeyboardInteractive,
    Agent,
}

impl AuthType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PublicKey => "publickey",
            Self::KeyboardInteractive => "keyboard-interactive",
            Self::Agent => "agent",
        }
    }

    fn parse(s: &str) -> Result<Self, StoreError> {
        match s {
            "password" => Ok(Self::Password),
            "publickey" => Ok(Self::PublicKey),
            "keyboard-interactive" => Ok(Self::KeyboardInteractive),
            "agent" => Ok(Self::Agent),
            other => Err(StoreError::Corrupt(format!("auth_type: {other}"))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    #[serde(rename = "username")]
    pub user: String,
    pub auth_type: AuthType,
    pub key_path: Option<String>,
    pub group_path: String,
    pub tags: Vec<String>,
    pub command: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct SessionRepo {
    pool: SqlitePool,
}

impl SessionRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id,name,host,port,username,auth_type,key_path,group_path,tags,command,created_at,updated_at
             FROM sessions ORDER BY group_path, name",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(row_to_record).collect()
    }

    pub async fn get(&self, id: &str) -> Result<SessionRecord, StoreError> {
        let row = sqlx::query(
            "SELECT id,name,host,port,username,auth_type,key_path,group_path,tags,command,created_at,updated_at
             FROM sessions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(db)?
        .ok_or_else(|| StoreError::NotFound(format!("session {id}")))?;
        row_to_record(&row)
    }

    /// 新建或更新（按 id 是否存在区分）；返回最终记录
    pub async fn upsert(&self, rec: &SessionRecord) -> Result<SessionRecord, StoreError> {
        let tags =
            serde_json::to_string(&rec.tags).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        sqlx::query(
            "INSERT INTO sessions (id,name,host,port,username,auth_type,key_path,group_path,tags,command,updated_at)
             VALUES (?,?,?,?,?,?,?,?,?,?,datetime('now'))
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, host=excluded.host, port=excluded.port,
               username=excluded.username, auth_type=excluded.auth_type,
               key_path=excluded.key_path, group_path=excluded.group_path,
               tags=excluded.tags, command=excluded.command, updated_at=datetime('now')",
        )
        .bind(&rec.id)
        .bind(&rec.name)
        .bind(&rec.host)
        .bind(rec.port as i64)
        .bind(&rec.user)
        .bind(rec.auth_type.as_str())
        .bind(&rec.key_path)
        .bind(&rec.group_path)
        .bind(tags)
        .bind(&rec.command)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        self.get(&rec.id).await
    }

    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM sessions WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("session {id}")));
        }
        Ok(())
    }
}

fn row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<SessionRecord, StoreError> {
    let tags_raw: String = row.get("tags");
    Ok(SessionRecord {
        id: row.get("id"),
        name: row.get("name"),
        host: row.get("host"),
        port: row.get::<i64, _>("port") as u16,
        user: row.get("username"),
        auth_type: AuthType::parse(row.get("auth_type"))?,
        key_path: row.get("key_path"),
        group_path: row.get("group_path"),
        tags: serde_json::from_str(&tags_raw).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        command: row.get("command"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

fn db(e: sqlx::Error) -> StoreError {
    StoreError::Query(e.to_string())
}
