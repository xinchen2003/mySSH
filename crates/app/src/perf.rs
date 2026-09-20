//! 性能指标导出（PR-0）：`perf_stats` 调试命令，汇聚各子系统内部量为 JSON。
//! 只读快照、零行为改变；供性能基线对比与退化排查。
//!
//! 未含 RSS/句柄数：需要新增 sysinfo 类依赖（依赖变更待批准），暂缺。

use std::sync::Arc;

use serde_json::{json, Value};

use crate::sftp::SftpManagerState;
use crate::terminal::TerminalManager;
use crate::tunnels::TunnelManagerState;

#[tauri::command]
pub async fn perf_stats(
    terminal: tauri::State<'_, Arc<TerminalManager>>,
    tunnels: tauri::State<'_, Arc<TunnelManagerState>>,
    sftp: tauri::State<'_, Arc<SftpManagerState>>,
) -> Result<Value, String> {
    // 仅开发模式（PR-0 定位：调试取数命令，不进发布面）
    if !cfg!(debug_assertions) {
        return Err("perf_stats 仅开发模式可用".into());
    }
    let tunnels_json: Vec<Value> = tunnels
        .mgr
        .list()
        .iter()
        .map(|t| {
            json!({
                "tunnelId": t.id,
                "kind": t.kind,
                "status": format!("{:?}", t.status),
                "activeConns": t.stats.active_conns,
                "totalConns": t.stats.total_conns,
                "rejectedConns": t.stats.rejected_conns,
                "bytesUp": t.stats.bytes_up,
                "bytesDown": t.stats.bytes_down,
                "errors": t.stats.errors,
                "reconnects": t.stats.reconnects,
            })
        })
        .collect();
    Ok(json!({
        "terminal": terminal.perf_json(),
        "sftp": sftp.perf_json(),
        "tunnels": tunnels_json,
    }))
}
