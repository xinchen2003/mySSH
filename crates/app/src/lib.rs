//! mySSH 应用后端。M0：壳 + 日志/panic；M1：终端会话（term_* 命令族）。
//! 命令契约见 docs/design/03-ipc-contract.md（逐里程碑补齐）。

mod files;
mod logging;
mod monitor;
mod sessions;
mod settings;
mod sftp;
mod terminal;
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
    let sftp_state = sftp::SftpManagerState::new();
    let monitor_state = monitor::MonitorState::new();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(Arc::new(TerminalManager::default()))
        .manage(session_state.clone())
        .manage(tunnel_mgr_state.clone())
        .manage(sftp_state)
        .manage(monitor_state)
        .setup(move |app| {
            // 开机自启隧道：store 已就绪，后台拉起（失败仅日志，监督器自持重连）
            let mgr = tunnel_mgr_state.mgr.clone();
            let store = session_state.store.clone();
            tauri::async_runtime::spawn(async move {
                crate::tunnels::autostart_tunnels(mgr, store).await;
            });
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            log_frontend,
            app_version,
            terminal::term_open,
            terminal::term_input,
            terminal::term_credit,
            terminal::term_resize,
            terminal::term_close,
            terminal::hostkey_confirm,
            terminal::ki_respond,
            files::read_private_key,
            files::open_local,
            files::local_mkdir,
            sessions::session_list,
            sessions::session_upsert,
            sessions::session_delete,
            sessions::cred_set,
            sessions::cred_delete,
            sessions::vault_status,
            sessions::import_openssh,
            sessions::config_export,
            sessions::config_import,
            sessions::import_putty,
            sessions::import_xshell,
            sessions::import_finalshell,
            tunnels::tunnel_start,
            tunnels::tunnel_stop,
            tunnels::tunnel_list,
            tunnels::tunnel_subscribe,
            tunnels::tunnel_save,
            tunnels::tunnel_delete,
            tunnels::tunnel_defs,
            sftp::sftp_list,
            sftp::sftp_stat,
            sftp::sftp_mkdir,
            sftp::sftp_delete,
            sftp::sftp_rename,
            sftp::sftp_chmod,
            sftp::local_list,
            sftp::sftp_upload,
            sftp::sftp_download,
            sftp::transfer_list,
            sftp::transfer_pause,
            sftp::transfer_resume,
            sftp::transfer_cancel,
            sftp::transfer_subscribe,
            sftp::sftp_edit_open,
            monitor::metrics_subscribe,
            monitor::metrics_unsubscribe,
            settings::settings_list,
            settings::settings_set,
            settings::settings_delete,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!(?e, "tauri exited");
            std::process::exit(1);
        });
}
