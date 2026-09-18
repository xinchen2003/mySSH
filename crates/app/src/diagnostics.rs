//! 一键导出诊断包（轴一 1.2）：日志目录 + 版本/系统信息打包 zip，用户报 bug 时给现场。
//!
//! 日志可能记录主机地址/用户名等连接信息——导出内容交由用户自行审阅（按钮文案已提示）。

use std::io::Write as _;

/// 打包日志与系统信息到指定 zip 路径（前端经保存对话框选址）。
#[tauri::command]
pub async fn export_diagnostics(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || write_bundle(&path))
        .await
        .map_err(|e| format!("导出任务失败: {e}"))?
}

fn write_bundle(path: &str) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("创建 {path} 失败: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();

    // 系统信息（时间取导出时刻的 UNIX 秒，避免引入日期库）
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let info = format!(
        "mySSH diagnostics\nversion: {}\nos: {} {}\nexported_epoch: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        epoch,
    );
    zip.start_file("system-info.txt", opts)
        .map_err(|e| format!("创建 system-info 条目失败: {e}"))?;
    zip.write_all(info.as_bytes())
        .map_err(|e| format!("写入 system-info 失败: {e}"))?;

    // 日志目录全量（滚动日志 myssh.log.* 与崩溃记录 crash-*.log）
    let dir = crate::logging::log_dir();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                let mut f = match std::fs::File::open(&p) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::warn!(file = %p.display(), "诊断包跳过不可读日志: {e}");
                        continue;
                    }
                };
                zip.start_file(format!("logs/{name}"), opts)
                    .map_err(|e| format!("创建 {name} 条目失败: {e}"))?;
                std::io::copy(&mut f, &mut zip).map_err(|e| format!("打包 {name} 失败: {e}"))?;
            }
        }
        Err(e) => {
            // 日志目录不存在（全新安装）：包内仍有 system-info，不为错误
            tracing::info!(dir = %dir.display(), "诊断包：日志目录不可读，仅导出系统信息: {e}");
        }
    }

    zip.finish().map_err(|e| format!("写入 zip 失败: {e}"))?;
    Ok(())
}
