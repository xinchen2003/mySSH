//! 性能指标导出（PR-0）：`perf_stats` 调试命令，汇聚各子系统内部量为 JSON。
//! 只读快照、零行为改变；供性能基线对比与退化排查。

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
        "process": process_json(),
    }))
}

/// 进程级指标：RSS/虚存（sysinfo，跨平台）。
/// 句柄数需 Win32 unsafe 调用，workspace forbid(unsafe_code) 下不可得，
/// 由采集脚本（scripts/perf-baseline.mjs）在进程外挂 PowerShell 补采。
/// 每次调用现取现弃（调试命令调用频率极低），不落全局状态。
fn process_json() -> Value {
    let pid = sysinfo::Pid::from_u32(std::process::id());
    let mut sys = sysinfo::System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), false);
    let proc = sys.process(pid);
    json!({
        "rssBytes": proc.map_or(0, |p| p.memory()),
        "virtualBytes": proc.map_or(0, |p| p.virtual_memory()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_json_reports_own_rss() {
        let v = process_json();
        assert!(
            v["rssBytes"].as_u64().unwrap_or(0) > 0,
            "本进程 RSS 必须非零"
        );
    }
}
