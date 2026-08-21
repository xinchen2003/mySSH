//! 日志与崩溃记录（M0 质量基座）。
//!
//! - tracing → 滚动文件：%LOCALAPPDATA%/myssh/logs/myssh.log.YYYY-MM-DD，保留 7 天
//! - panic hook → 崩溃现场落盘 %LOCALAPPDATA%/myssh/logs/crash-*.log（backtrace + 时间戳）
//! - 前端错误经 `log_frontend` 命令汇流到同一日志

use std::backtrace::Backtrace;
use std::path::PathBuf;

pub fn log_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("myssh").join("logs")
}

/// 初始化日志 + panic 记录。返回的 WorkerGuard 必须持有到进程结束（drop 即冲刷停写）。
pub fn init() -> tracing_appender::non_blocking::WorkerGuard {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).ok();

    let file_appender = tracing_appender::rolling::daily(&dir, "myssh.log");
    let (writer, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,russh=warn".into()),
        )
        .with_writer(writer)
        .with_ansi(false)
        .init();

    install_panic_hook(dir);
    guard
}

fn install_panic_hook(dir: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = Backtrace::force_capture();
        let payload = info.payload();
        let msg = if let Some(s) = payload.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string payload>".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let body = format!("panic at {location}\nmessage: {msg}\n\nbacktrace:\n{backtrace}\n");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = dir.join(format!("crash-{ts}.log"));
        if std::fs::write(&path, &body).is_err() {
            eprintln!("{body}");
        }
        tracing::error!(target: "panic", "panic at {location}: {msg}");
    }));
}
