//! mySSH 应用后端。M0：壳 + 日志/panic；M1：终端会话（term_* 命令族）。
//! 命令契约见 docs/design/03-ipc-contract.md（逐里程碑补齐）。

mod connect;
mod diagnostics;
mod encoding;
mod exec;
mod files;
mod fs_limiter;
mod governor;
mod local_pty;
mod logging;
mod mcp;
mod mcp_terminal;
mod monitor;
mod perf;
mod sessions;
mod settings;
mod sftp;
mod stream_protocol;
mod terminal;
mod transfer_ledger;
mod tunnels;

use std::sync::Arc;

use sessions::SessionManagerState;
use terminal::TerminalManager;
use tracing::info;
use tunnels::TunnelManagerState;

#[tauri::command]
async fn log_frontend(msg: String) -> Result<(), String> {
    info!(target: "frontend", "{msg}");
    Ok(())
}

#[tauri::command]
async fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn run() {
    let _log_guard = logging::init();
    info!("mySSH starting");

    // 会话存储打不开属环境级故障：fail loud（M2 起会话/隧道/审计全依赖它）
    let store = tauri::async_runtime::block_on(core_store::Store::open(&sessions::store_path()))
        .unwrap_or_else(|e| {
            tracing::error!(error = %e, "会话存储打开失败，终止");
            eprintln!("会话存储打开失败: {e}");
            std::process::exit(1);
        });
    let session_state = Arc::new(SessionManagerState {
        store: Arc::new(store),
    });
    let tunnel_mgr_state = Arc::new(TunnelManagerState {
        mgr: core_tunnel::TunnelManager::new(),
        last: parking_lot::Mutex::new(std::collections::HashMap::new()),
    });
    let sftp_state = sftp::SftpManagerState::new(session_state.store.clone());
    let exec_state = exec::ExecManagerState::new(sftp_state.rt());
    let monitor_state = monitor::MonitorState::new();
    let mcp_manager = mcp::McpManager::new();
    let mcp_terms = mcp_terminal::McpTerminalState::new();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(Arc::new(TerminalManager::default()))
        .manage(session_state.clone())
        .manage(tunnel_mgr_state.clone())
        .manage(sftp_state.clone())
        .manage(exec_state.clone())
        .manage(monitor_state)
        .manage(mcp_manager.clone())
        .manage(mcp_terms.clone())
        .setup(move |app| {
            // 开机自启隧道：store 已就绪，后台拉起（失败仅日志，监督器自持重连）
            let mgr = tunnel_mgr_state.mgr.clone();
            let store = session_state.store.clone();
            tauri::async_runtime::spawn(async move {
                crate::tunnels::autostart_tunnels(mgr, store).await;
            });
            // 审计保留策略：audit 表只增不减，启动时删 90 天前行（失败仅日志）
            let store = session_state.store.clone();
            tauri::async_runtime::spawn(async move {
                match store.audit().prune_older_than(90).await {
                    Ok(0) => {}
                    Ok(n) => tracing::info!(pruned = n, "审计保留策略：已清理 90 天前记录"),
                    Err(e) => tracing::warn!(error = %e, "审计清理失败"),
                }
            });
            // MCP 服务端：按 mcp.enabled/port/token 设置启动（绑定失败仅日志）
            let store = session_state.store.clone();
            let sftp = sftp_state.clone();
            let exec = exec_state.clone();
            let terms = mcp_terms.clone();
            tauri::async_runtime::spawn(async move {
                crate::mcp::boot_from_settings(mcp_manager, store, sftp, exec, terms).await;
            });
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            log_frontend,
            app_version,
            perf::perf_stats,
            diagnostics::export_diagnostics,
            terminal::term_open,
            terminal::term_input,
            terminal::term_input_raw,
            terminal::term_credit,
            terminal::term_focus,
            terminal::term_resize,
            terminal::term_close,
            terminal::hostkey_confirm,
            terminal::ki_respond,
            files::read_private_key,
            files::open_local,
            files::local_mkdir,
            files::local_stat,
            files::local_touch,
            files::local_rename,
            files::local_delete,
            files::open_in_explorer,
            files::local_copy,
            files::local_desktop_path,
            files::transfer_save_file,
            sessions::session_list,
            sessions::session_upsert,
            sessions::session_delete,
            sessions::group_rename,
            sessions::group_delete,
            sessions::session_move,
            sessions::cred_set,
            sessions::cred_delete,
            sessions::vault_status,
            sessions::session_test_connect,
            sessions::config_export,
            sessions::config_import,
            tunnels::tunnel_start,
            tunnels::tunnel_stop,
            tunnels::tunnel_list,
            tunnels::tunnel_subscribe,
            tunnels::tunnel_save,
            tunnels::tunnel_check_port,
            tunnels::tunnel_delete,
            tunnels::tunnel_defs,
            sftp::sftp_list,
            sftp::sftp_stat,
            sftp::sftp_mkdir,
            sftp::sftp_delete,
            sftp::sftp_rename,
            sftp::sftp_chmod,
            sftp::sftp_touch,
            sftp::sftp_home,
            sftp::shell_integration_status,
            sftp::shell_integration_set,
            sftp::local_list,
            sftp::sftp_upload,
            sftp::sftp_download,
            sftp::transfer_list,
            sftp::transfer_pause,
            sftp::transfer_resume,
            sftp::transfer_cancel,
            sftp::transfer_retry,
            sftp::transfer_remove,
            sftp::transfer_prioritize,
            sftp::transfer_clear,
            sftp::transfer_pause_all,
            sftp::transfer_resume_all,
            sftp::transfer_subscribe,
            sftp::transfer_job_list,
            sftp::transfer_job_pause,
            sftp::transfer_job_resume,
            sftp::transfer_job_cancel,
            sftp::transfer_job_retry,
            sftp::transfer_job_remove,
            sftp::transfer_history,
            sftp::transfer_history_clear,
            sftp::sftp_edit_open,
            monitor::metrics_subscribe,
            monitor::metrics_unsubscribe,
            settings::settings_list,
            settings::settings_set,
            settings::settings_delete,
            mcp::mcp_status,
            mcp::mcp_restart,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!(?e, "tauri exited");
            std::process::exit(1);
        });
}
