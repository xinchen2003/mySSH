//! 设置命令族：settings_list / settings_set / settings_delete（M5）。
//! KV 透传（值 JSON 文本）；键白名单约束在前端常量，后端只拒空键。

use std::sync::Arc;

use serde_json::{json, Value};

use crate::sessions::SessionManagerState;

#[tauri::command]
pub async fn settings_list(
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let all = sessions
        .store
        .settings()
        .all()
        .await
        .map_err(|e| e.to_string())?;
    let map: serde_json::Map<String, Value> = all
        .into_iter()
        .filter_map(|(k, v)| serde_json::from_str::<Value>(&v).ok().map(|jv| (k, jv)))
        .collect();
    Ok(json!({ "settings": Value::Object(map) }))
}

#[tauri::command]
pub async fn settings_set(
    key: String,
    value: Value,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    if key.is_empty()
        || key.len() > 64
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return Err("非法设置键".into());
    }
    let text = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    sessions
        .store
        .settings()
        .set(&key, &text)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn settings_delete(
    key: String,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    sessions
        .store
        .settings()
        .delete(&key)
        .await
        .map_err(|e| e.to_string())
}
