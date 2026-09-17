//! AI 审计查询命令：设置弹窗「AI 审计」区块的只读时间线数据源。
//! audit 表 append-only（core-store 已保证），这里只做游标分页查询。

use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;

use crate::sessions::SessionManagerState;

/// 单条审计记录的序列化镜像（core_store::AuditRecord 不带 serde derive，此处独立 DTO）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecordDto {
    id: i64,
    ts: String,
    actor: String,
    session_id: Option<String>,
    action: String,
    detail: Value,
}

/// 一页审计记录：按 id 倒序，next_cursor 为下一页游标（None = 没有更多）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditPage {
    records: Vec<AuditRecordDto>,
    next_cursor: Option<i64>,
}

/// 审计时间线查询：limit 缺省 50、上限 200（clamp）
#[tauri::command]
pub async fn audit_query(
    state: tauri::State<'_, Arc<SessionManagerState>>,
    cursor: Option<i64>,
    limit: Option<u32>,
) -> Result<AuditPage, String> {
    let limit = limit.unwrap_or(50).clamp(1, 200);
    let (records, next_cursor) = state
        .store
        .audit()
        .query(cursor, limit)
        .await
        .map_err(|e| e.to_string())?;
    Ok(AuditPage {
        records: records
            .into_iter()
            .map(|r| AuditRecordDto {
                id: r.id,
                ts: r.ts,
                actor: r.actor,
                session_id: r.session_id,
                action: r.action,
                detail: r.detail,
            })
            .collect(),
        next_cursor,
    })
}
