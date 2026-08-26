//! 传输历史仓储：transfers 表（M3）。断点续传状态落库，跨重启可按记录重入队。

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::error::StoreError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRecord {
    pub id: String,
    pub session_id: String,
    pub direction: String, // upload | download
    pub local: String,
    pub remote: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub state: String, // queued|running|paused|done|failed|canceled
    pub error: Option<String>,
    pub updated_at: String,
}

pub struct TransferRepo {
    pool: SqlitePool,
}

impl TransferRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// 幂等 upsert（按主键覆盖）；updated_at 由 SQLite 时钟生成（与 sessions 表同约定）
    pub async fn upsert(&self, r: &TransferRecord) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO transfers (id,session_id,direction,local,remote,bytes_done,bytes_total,state,error,updated_at)
             VALUES (?,?,?,?,?,?,?,?,?,datetime('now'))
             ON CONFLICT(id) DO UPDATE SET bytes_done=excluded.bytes_done,
               bytes_total=excluded.bytes_total,state=excluded.state,
               error=excluded.error,updated_at=datetime('now')",
        )
        .bind(&r.id)
        .bind(&r.session_id)
        .bind(&r.direction)
        .bind(&r.local)
        .bind(&r.remote)
        .bind(r.bytes_done as i64)
        .bind(r.bytes_total as i64)
        .bind(&r.state)
        .bind(&r.error)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub async fn for_session(&self, session_id: &str) -> Result<Vec<TransferRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id,session_id,direction,local,remote,bytes_done,bytes_total,state,error,updated_at
             FROM transfers WHERE session_id = ? ORDER BY updated_at DESC LIMIT 200",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(row_to_record).collect()
    }

    /// 清理某会话的已完成/已取消历史（保留失败与进行中）
    pub async fn clear_settled(&self, session_id: &str) -> Result<u64, StoreError> {
        let r = sqlx::query(
            "DELETE FROM transfers WHERE session_id = ? AND state IN ('done','canceled')",
        )
        .bind(session_id)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(r.rows_affected())
    }

    /// 全部会话的历史记录（终态落表），按更新时间倒序
    pub async fn recent(&self, limit: u32) -> Result<Vec<TransferRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id,session_id,direction,local,remote,bytes_done,bytes_total,state,error,updated_at
             FROM transfers ORDER BY updated_at DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(row_to_record).collect()
    }

    /// 清空全部历史记录
    pub async fn clear_all(&self) -> Result<u64, StoreError> {
        let r = sqlx::query("DELETE FROM transfers")
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(r.rows_affected())
    }
}

fn row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<TransferRecord, StoreError> {
    Ok(TransferRecord {
        id: row.try_get("id").map_err(db)?,
        session_id: row.try_get("session_id").map_err(db)?,
        direction: row.try_get("direction").map_err(db)?,
        local: row.try_get("local").map_err(db)?,
        remote: row.try_get("remote").map_err(db)?,
        bytes_done: row.try_get::<i64, _>("bytes_done").map_err(db)? as u64,
        bytes_total: row.try_get::<i64, _>("bytes_total").map_err(db)? as u64,
        state: row.try_get("state").map_err(db)?,
        error: row.try_get("error").map_err(db)?,
        updated_at: row.try_get("updated_at").map_err(db)?,
    })
}

fn db(e: sqlx::Error) -> StoreError {
    StoreError::Query(e.to_string())
}
