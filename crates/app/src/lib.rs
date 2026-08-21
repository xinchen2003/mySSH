//! mySSH 应用后端。M0：壳 + 日志/panic + 最小命令面。
//! 命令契约见 docs/design/03-ipc-contract.md（逐里程碑补齐）。

mod logging;

use tracing::info;

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

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![log_frontend, app_version])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            tracing::error!(?e, "tauri exited");
            std::process::exit(1);
        });
}
