//! 应用设置仓储：settings KV 表（M5）。值统一存 JSON 文本，解析由调用方负责。

use sqlx::SqlitePool;

use crate::error::StoreError;

pub struct SettingsRepo {
    pool: SqlitePool,
}

impl SettingsRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<String>, StoreError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?;
        Ok(row.map(|r| r.0))
    }

    /// 全量导出（前端一次性拉取后本地渲染）
    pub async fn all(&self) -> Result<Vec<(String, String)>, StoreError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM settings ORDER BY key")
                .fetch_all(&self.pool)
                .await
                .map_err(db)?;
        Ok(rows)
    }

    pub async fn set(&self, key: &str, value: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO settings (key,value,updated_at) VALUES (?,?,datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value=excluded.value,updated_at=datetime('now')",
        )
        .bind(key)
        .bind(value)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub async fn delete(&self, key: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }
}

fn db(e: sqlx::Error) -> StoreError {
    StoreError::Query(e.to_string())
}
