//! 一键导出诊断包（轴一 1.2）：日志目录 + 版本/系统信息打包 zip，用户报 bug 时给现场。
//!
//! 日志可能记录主机地址/用户名等连接信息——导出内容交由用户自行审阅（按钮文案已提示）。
//! 时间范围（since_epoch）：crash 文件按文件名 epoch 取舍；滚动日志按条目
//! 前导 RFC3339 时间戳逐条过滤（多行条目的延续行继承首行取舍）；mtime 早于
//! 起点的文件整体跳过。

use std::io::BufRead as _;
use std::io::Write as _;

/// 打包日志与系统信息到指定 zip 路径（前端经保存对话框选址）。
/// since_epoch = Some(ts) 时只收 ts 之后的日志；None 全量。
#[tauri::command]
pub async fn export_diagnostics(path: String, since_epoch: Option<u64>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || write_bundle(&path, since_epoch))
        .await
        .map_err(|e| format!("导出任务失败: {e}"))?
}

fn write_bundle(path: &str, since_epoch: Option<u64>) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("创建 {path} 失败: {e}"))?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default();

    // 系统信息（时间取导出时刻的 UNIX 秒，避免引入日期库）
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let info =
        format!(
        "mySSH diagnostics\nversion: {}\nos: {} {}\nexported_epoch: {}\nrange_since_epoch: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        epoch,
        since_epoch.map(|s| s.to_string()).unwrap_or_else(|| "all".into()),
    );
    zip.start_file("system-info.txt", opts)
        .map_err(|e| format!("创建 system-info 条目失败: {e}"))?;
    zip.write_all(info.as_bytes())
        .map_err(|e| format!("写入 system-info 失败: {e}"))?;

    // 日志目录（滚动日志 myssh.log.* 与崩溃记录 crash-*.log），按时间范围过滤
    let dir = crate::logging::log_dir();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().into_owned();
                match file_cut(&name, &p, since_epoch) {
                    Cut::Skip => {}
                    Cut::Whole => pack_whole(&mut zip, &p, &name, opts)?,
                    Cut::Since(since) => pack_since(&mut zip, &p, &name, since, opts)?,
                }
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

/// 单文件取舍：全量 / 整体跳过 / 逐行过滤
enum Cut {
    Skip,
    Whole,
    Since(u64),
}

fn file_cut(name: &str, path: &std::path::Path, since_epoch: Option<u64>) -> Cut {
    let Some(since) = since_epoch else {
        return Cut::Whole;
    };
    // crash-{epoch}.log：文件名即落盘时刻
    if let Some(ts) = name
        .strip_prefix("crash-")
        .and_then(|s| s.strip_suffix(".log"))
        .and_then(|s| s.parse::<u64>().ok())
    {
        return if ts >= since { Cut::Whole } else { Cut::Skip };
    }
    // 滚动日志：mtime 早于起点 → 绝无新内容，整体跳过；否则逐行过滤
    if name.starts_with("myssh.log") {
        let mtime = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        return if mtime < since {
            Cut::Skip
        } else {
            Cut::Since(since)
        };
    }
    // 未知文件：保守全收
    Cut::Whole
}

fn pack_whole(
    zip: &mut zip::ZipWriter<std::fs::File>,
    path: &std::path::Path,
    name: &str,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(file = %path.display(), "诊断包跳过不可读日志: {e}");
            return Ok(());
        }
    };
    zip.start_file(format!("logs/{name}"), opts)
        .map_err(|e| format!("创建 {name} 条目失败: {e}"))?;
    std::io::copy(&mut f, zip).map_err(|e| format!("打包 {name} 失败: {e}"))?;
    Ok(())
}

/// 逐行过滤：条目首行带前导 RFC3339 时间戳（tracing fmt 默认格式）；
/// 无时间戳的行视为上一条目延续（backtrace 等），继承其取舍。
/// 首条达标条目出现才建 zip 条目——范围内无内容的文件不进包。
fn pack_since(
    zip: &mut zip::ZipWriter<std::fs::File>,
    path: &std::path::Path,
    name: &str,
    since: u64,
    opts: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    let f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(file = %path.display(), "诊断包跳过不可读日志: {e}");
            return Ok(());
        }
    };
    let mut started = false;
    let mut keeping = false;
    for line in std::io::BufReader::new(f).lines() {
        let line = line.map_err(|e| format!("读取 {name} 失败: {e}"))?;
        if let Some(ts) = parse_entry_ts(&line) {
            keeping = ts >= since;
        }
        if keeping {
            if !started {
                zip.start_file(format!("logs/{name}"), opts)
                    .map_err(|e| format!("创建 {name} 条目失败: {e}"))?;
                started = true;
            }
            zip.write_all(line.as_bytes())
                .and_then(|()| zip.write_all(b"\n"))
                .map_err(|e| format!("打包 {name} 失败: {e}"))?;
        }
    }
    Ok(())
}

/// 解析条目首行前导时间戳：`YYYY-MM-DDTHH:MM:SS`（UTC），返回 UNIX 秒。
/// 位置严格匹配，任何偏差（延续行、旧格式）→ None。
fn parse_entry_ts(line: &str) -> Option<u64> {
    let b = line.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let y = line.get(0..4)?.parse::<i64>().ok()?;
    let mo = line.get(5..7)?.parse::<u32>().ok()?;
    let d = line.get(8..10)?.parse::<u32>().ok()?;
    let h = line.get(11..13)?.parse::<u64>().ok()?;
    let mi = line.get(14..16)?.parse::<u64>().ok()?;
    let s = line.get(17..19)?.parse::<u64>().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    u64::try_from(days * 86400 + (h * 3600 + mi * 60 + s) as i64).ok()
}

/// civil 日期 → 1970-01-01 起天数（Hinnant 算法；公历全区间有效）
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12; // 3 月=0 … 1 月=10，2 月=11
    let doy = (153 * i64::from(mp) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_epoch_anchors() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        // 2026-01-01 00:00:00 UTC = 1767225600
        assert_eq!(days_from_civil(2026, 1, 1) * 86400, 1767225600);
        // 闰日：2024-02-29 与 2024-03-01 相邻
        assert_eq!(
            days_from_civil(2024, 3, 1) - days_from_civil(2024, 2, 29),
            1
        );
    }

    #[test]
    fn parse_entry_ts_reads_rfc3339_prefix() {
        // 2026-09-22T07:16:25Z = 1767225600 + 264*86400 + 26185
        let ts = parse_entry_ts("2026-09-22T07:16:25.123456Z  INFO app: hello").unwrap();
        assert_eq!(ts, 1767225600 + 264 * 86400 + 7 * 3600 + 16 * 60 + 25);
    }

    #[test]
    fn parse_entry_ts_rejects_continuation_and_garbage() {
        assert_eq!(parse_entry_ts("  at core::panicking"), None);
        assert_eq!(parse_entry_ts(""), None);
        assert_eq!(parse_entry_ts("2026-13-01T00:00:00Z bad month"), None);
        assert_eq!(parse_entry_ts("not-a-timestamp line"), None);
    }

    #[test]
    fn pack_since_filters_entries_and_inherits_continuation() {
        let dir = std::env::temp_dir().join(format!("diag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("myssh.log.2026-09-22");
        std::fs::write(
            &log,
            "2026-09-22T06:00:00.000Z  INFO old entry\n\
             continuation of old\n\
             2026-09-22T08:00:00.000Z  INFO new entry\n\
             continuation of new\n",
        )
        .unwrap();
        let zip_path = dir.join("out.zip");
        let zip_file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(zip_file);
        // since = 07:00 → 只收 08:00 条目及其延续行
        let since = parse_entry_ts("2026-09-22T07:00:00Z").unwrap();
        pack_since(
            &mut zip,
            &log,
            "myssh.log.2026-09-22",
            since,
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        // File::create 为只写句柄：归档读取须另行打开（Windows 上对只写句柄 seek 会 EACCES）
        drop(zip.finish().unwrap());
        let mut archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        let mut entry = archive.by_name("logs/myssh.log.2026-09-22").unwrap();
        let mut body = String::new();
        std::io::Read::read_to_string(&mut entry, &mut body).unwrap();
        assert_eq!(
            body,
            "2026-09-22T08:00:00.000Z  INFO new entry\ncontinuation of new\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn pack_since_skips_file_without_in_range_entries() {
        let dir = std::env::temp_dir().join(format!("diag-test-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("myssh.log.2026-09-20");
        std::fs::write(&log, "2026-09-20T06:00:00.000Z  INFO old\n").unwrap();
        let zip_path = dir.join("out.zip");
        let zip_file = std::fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(zip_file);
        let since = parse_entry_ts("2026-09-22T07:00:00Z").unwrap();
        pack_since(
            &mut zip,
            &log,
            "myssh.log.2026-09-20",
            since,
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        drop(zip.finish().unwrap());
        let archive = zip::ZipArchive::new(std::fs::File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<String> = archive.file_names().map(str::to_string).collect();
        assert!(names.is_empty(), "范围内无条目的文件不应进包: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
