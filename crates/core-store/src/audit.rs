//! append-only 审计（安全模型第 5 条）：只提供 append 与 query，无 UPDATE/DELETE。

use serde::Serialize;
use serde_json::Value;
use sqlx::{Row, SqlitePool};

use crate::error::StoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Gui,
    Cli,
    Mcp,
}

impl Actor {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Gui => "gui",
            Self::Cli => "cli",
            Self::Mcp => "mcp",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuditRecord {
    pub id: i64,
    pub ts: String,
    pub actor: String,
    pub session_id: Option<String>,
    pub action: String,
    pub detail: Value,
}

pub struct AuditRepo {
    pool: SqlitePool,
}

impl AuditRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn append(
        &self,
        actor: Actor,
        session_id: Option<&str>,
        action: &str,
        detail: &impl Serialize,
    ) -> Result<(), StoreError> {
        let detail =
            serde_json::to_value(detail).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        sqlx::query("INSERT INTO audit (actor, session_id, action, detail) VALUES (?,?,?,?)")
            .bind(actor.as_str())
            .bind(session_id)
            .bind(action)
            .bind(detail.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?;
        Ok(())
    }

    /// 游标分页：按 id 倒序，cursor 为上一页末条 id
    pub async fn query(
        &self,
        cursor: Option<i64>,
        limit: u32,
    ) -> Result<(Vec<AuditRecord>, Option<i64>), StoreError> {
        let limit = limit.clamp(1, 500) as i64;
        let rows = match cursor {
            Some(c) => sqlx::query(
                "SELECT id,ts,actor,session_id,action,detail FROM audit
                 WHERE id < ? ORDER BY id DESC LIMIT ?",
            )
            .bind(c)
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?,
            None => sqlx::query(
                "SELECT id,ts,actor,session_id,action,detail FROM audit
                 ORDER BY id DESC LIMIT ?",
            )
            .bind(limit + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| StoreError::Query(e.to_string()))?,
        };
        let mut records: Vec<AuditRecord> = rows
            .iter()
            .map(|r| {
                let detail_raw: String = r.get("detail");
                AuditRecord {
                    id: r.get("id"),
                    ts: r.get("ts"),
                    actor: r.get("actor"),
                    session_id: r.get("session_id"),
                    action: r.get("action"),
                    detail: serde_json::from_str(&detail_raw).unwrap_or(Value::Null),
                }
            })
            .collect();
        let next_cursor = if records.len() as i64 > limit {
            records.pop().map(|r| r.id)
        } else {
            None
        };
        Ok((records, next_cursor))
    }
}
