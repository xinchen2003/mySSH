//! 隧道定义仓储：tunnels 表 CRUD（持久化 + 自启/随会话标记，M2 收口）。

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

use crate::error::StoreError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelRecord {
    pub id: String,
    pub session_id: String,
    pub kind: String, // local|remote|dynamic
    /// 显示名（模板预填；可空 → 面板回退为绑定地址）
    #[serde(default)]
    pub name: String,
    pub bind_host: String,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    /// 开机自启（app 启动即建立）
    pub autostart: bool,
    /// 随会话自动建立（该会话终端连接成功后拉起）
    pub with_session: bool,
    pub created_at: String,
}

pub struct TunnelRepo {
    pool: SqlitePool,
}

impl TunnelRepo {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<TunnelRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id,session_id,kind,name,bind_host,bind_port,target_host,target_port,autostart,with_session,created_at
             FROM tunnels ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(row_to_record).collect()
    }

    pub async fn for_session(&self, session_id: &str) -> Result<Vec<TunnelRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT id,session_id,kind,name,bind_host,bind_port,target_host,target_port,autostart,with_session,created_at
             FROM tunnels WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.iter().map(row_to_record).collect()
    }

    pub async fn upsert(&self, rec: &TunnelRecord) -> Result<(), StoreError> {
        validate(rec)?;
        sqlx::query(
            "INSERT INTO tunnels (id,session_id,kind,name,bind_host,bind_port,target_host,target_port,autostart,with_session)
             VALUES (?,?,?,?,?,?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET
               session_id=excluded.session_id, kind=excluded.kind, name=excluded.name,
               bind_host=excluded.bind_host, bind_port=excluded.bind_port,
               target_host=excluded.target_host, target_port=excluded.target_port,
               autostart=excluded.autostart, with_session=excluded.with_session",
        )
        .bind(&rec.id)
        .bind(&rec.session_id)
        .bind(&rec.kind)
        .bind(&rec.name)
        .bind(&rec.bind_host)
        .bind(rec.bind_port as i64)
        .bind(&rec.target_host)
        .bind(rec.target_port.map(|p| p as i64))
        .bind(rec.autostart as i64)
        .bind(rec.with_session as i64)
        .execute(&self.pool)
        .await
        .map_err(db)?;
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), StoreError> {
        let res = sqlx::query("DELETE FROM tunnels WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound(format!("tunnel {id}")));
        }
        Ok(())
    }
}

fn row_to_record(row: &sqlx::sqlite::SqliteRow) -> Result<TunnelRecord, StoreError> {
    Ok(TunnelRecord {
        id: row.get("id"),
        session_id: row.get("session_id"),
        kind: row.get("kind"),
        name: row.get("name"),
        bind_host: row.get("bind_host"),
        bind_port: row.get::<i64, _>("bind_port") as u16,
        target_host: row.get("target_host"),
        target_port: row.get::<Option<i64>, _>("target_port").map(|p| p as u16),
        autostart: row.get::<i64, _>("autostart") != 0,
        with_session: row.get::<i64, _>("with_session") != 0,
        created_at: row.get("created_at"),
    })
}

fn db(e: sqlx::Error) -> StoreError {
    StoreError::Query(e.to_string())
}
/// 写入口径校验（upsert 统一调用，覆盖面板保存与配置导入两条路径）
fn validate(rec: &TunnelRecord) -> Result<(), StoreError> {
    if rec.id.trim().is_empty() {
        return Err(StoreError::Validation("隧道 id 不能为空".into()));
    }
    if !matches!(rec.kind.as_str(), "local" | "remote" | "dynamic") {
        return Err(StoreError::Validation(format!(
            "未知隧道类型 {}（local|remote|dynamic）",
            rec.kind
        )));
    }
    if rec.bind_host.trim().is_empty() {
        return Err(StoreError::Validation("绑定地址不能为空".into()));
    }
    if rec.bind_port == 0 {
        return Err(StoreError::Validation("绑定端口必须在 1-65535".into()));
    }
    if rec.kind != "dynamic" {
        match (&rec.target_host, rec.target_port) {
            (Some(h), Some(p)) if !h.trim().is_empty() && p > 0 => {}
            _ => {
                return Err(StoreError::Validation(
                    "local/remote 隧道需要目标地址与端口（1-65535）".into(),
                ));
            }
        }
    }
    Ok(())
}
