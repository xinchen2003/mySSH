//! 监控命令族：metrics_subscribe / metrics_unsubscribe（M4）。
//!
//! - 复用 SFTP 的 Bulk 连接（同连接开独立 exec channel，不占交互连接）
//! - 采集循环跑在 bulk-rt；每轮一个 snapshot 经 ipc::Channel 推给前端
//! - 降级：NoProcfs 推一次 error 即退出；其余错误连错 3 轮退出，前端可重订

use std::collections::HashMap;
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::ipc::Channel;

use core_monitor::MonitorError;

use crate::sessions::SessionManagerState;
use crate::sftp::SftpManagerState;

#[derive(Default)]
pub struct MonitorState {
    subs: Mutex<HashMap<String, tokio::task::AbortHandle>>,
}

impl MonitorState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

fn unsubscribe_inner(state: &MonitorState, session_id: &str) {
    if let Some(h) = state.subs.lock().remove(session_id) {
        h.abort();
    }
}

#[tauri::command]
pub async fn metrics_subscribe(
    session_id: String,
    interval_ms: u32,
    events: Channel<Value>,
    state: tauri::State<'_, Arc<MonitorState>>,
    sftp: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    unsubscribe_inner(&state, &session_id);
    let interval = u64::from(interval_ms.max(2000)); // 契约：下限 2s
    let ctx = crate::sftp::ensure_ctx(&sftp, &sessions.store, &session_id).await?;
    crate::sftp::audit(&sessions.store, &session_id, "metrics_subscribe", "").await;
    let rt = sftp.rt();
    let join = rt.spawn(async move {
        let mut collector = core_monitor::MetricsCollector::new();
        let mut errs = 0u32;
        loop {
            match collector.collect(ctx.conn()).await {
                Ok(snap) => {
                    errs = 0;
                    if events
                        .send(json!({ "kind": "snapshot", "data": snap }))
                        .is_err()
                    {
                        break; // 前端退订/窗口销毁
                    }
                }
                Err(e) => {
                    let fatal = matches!(e, MonitorError::NoProcfs);
                    errs += 1;
                    let _ = events.send(json!({
                        "kind": "error",
                        "message": e.to_string(),
                        "fatal": fatal,
                    }));
                    if fatal || errs >= 3 || ctx.conn().is_closed() {
                        break;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
        }
    });
    state.subs.lock().insert(session_id, join.abort_handle());
    Ok(())
}

#[tauri::command]
pub async fn metrics_unsubscribe(
    session_id: String,
    state: tauri::State<'_, Arc<MonitorState>>,
) -> Result<(), String> {
    unsubscribe_inner(&state, &session_id);
    Ok(())
}
