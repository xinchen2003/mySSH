//! 受限文件读取：连接对话框选择私钥文件后读入内存（不持久化，Zeroizing 由 core-ssh 负责）。
use serde_json::{json, Value};

/// 私钥文件大小上限（PuTTY/OpenSSH 私钥均在数 KB 量级）
const MAX_KEY_BYTES: u64 = 64 * 1024;

/// 读取私钥文件内容。限制：
/// - 扩展名属 {.ppk, .pem, .key} 或文件名以 id_ 开头（OpenSSH 默认命名）；
/// - ≤64KB；UTF-8 文本。
/// 不做任意文件读取——缩小本地攻击面。
#[tauri::command]
pub async fn read_private_key(path: String) -> Result<String, String> {
    let p = std::path::Path::new(&path);
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "无效文件名".to_string())?;
    let ext_ok = p
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "ppk" | "pem" | "key"))
        .unwrap_or(false);
    if !(ext_ok || name.starts_with("id_")) {
        return Err("不是可识别的私钥文件（.ppk/.pem/.key 或 id_*）".into());
    }
    let meta = std::fs::metadata(p).map_err(|e| format!("读取失败: {e}"))?;
    if meta.len() > MAX_KEY_BYTES {
        return Err("文件过大，不是私钥".into());
    }
    std::fs::read_to_string(p).map_err(|e| format!("读取失败: {e}"))
}

/// 系统默认方式打开本地路径（SFTP 直编的临时文件 → 默认编辑器）。
/// 只允许打开 %TEMP%/myssh-edit-* 下的文件——本命令存在的唯一场景。
#[tauri::command]
pub async fn open_local(path: String) -> Result<(), String> {
    let p = std::path::PathBuf::from(&path);
    let temp = std::env::temp_dir();
    let in_edit_area = p
        .parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with("myssh-edit-"))
        .unwrap_or(false)
        && p.starts_with(&temp);
    if !in_edit_area {
        return Err("仅允许打开 myssh-edit 临时区文件".into());
    }
    std::process::Command::new("cmd")
        .args(["/c", "start", "", &path])
        .spawn()
        .map_err(|e| format!("打开失败: {e}"))?;
    Ok(())
}

/// 本地新建目录（SFTP 面板本地栏操作；用户本机文件管理语义，路径不做白名单）
#[tauri::command]
pub async fn local_mkdir(path: String) -> Result<(), String> {
    std::fs::create_dir_all(&path).map_err(|e| format!("创建 {path} 失败: {e}"))
}

/// 本地元数据探测（冲突检测/新建前检查用；只读元数据，不读内容）。
/// 与 local_mkdir 同级：本机文件管理语义，路径不做白名单。
#[tauri::command]
pub async fn local_stat(path: String) -> Result<Value, String> {
    match std::fs::metadata(&path) {
        Ok(m) => Ok(json!({ "exists": true, "size": m.len(), "isDir": m.is_dir() })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(json!({ "exists": false, "size": 0, "isDir": false }))
        }
        Err(e) => Err(io_humanize(&e)),
    }
}

/// 本地新建空文件（create_new 语义：已存在则报错，绝不截断；父目录须已存在）
#[tauri::command]
pub async fn local_touch(path: String) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map(|_| ())
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                format!("目标已存在: {path}")
            } else {
                io_humanize(&e)
            }
        })
}
/// io 错误可读化：权限/占用给中文提示，其余保留原始错误
fn io_humanize(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!("权限不足: {e}")
    } else if e.raw_os_error() == Some(32) {
        // Windows ERROR_SHARING_VIOLATION：文件被其他进程占用
        format!("文件被占用: {e}")
    } else {
        e.to_string()
    }
}

/// 本地重命名/移动（SFTP 面板本地栏）；目标已存在则拒绝，避免静默覆盖
#[tauri::command]
pub async fn local_rename(from: String, to: String) -> Result<(), String> {
    if std::path::Path::new(&to).exists() {
        return Err(format!("目标已存在: {to}"));
    }
    std::fs::rename(&from, &to).map_err(|e| io_humanize(&e))
}

/// 本地删除：文件 remove_file / 目录 remove_dir_all（递归）
#[tauri::command]
pub async fn local_delete(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    let meta = std::fs::metadata(p).map_err(|e| io_humanize(&e))?;
    let r = if meta.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
    r.map_err(|e| io_humanize(&e))
}
/// 本地复制（SFTP 面板：OS 文件拖入本地栏 = 复制进当前本地目录）。
/// 目录递归复制；目标已存在则拒绝，避免静默覆盖。
#[tauri::command]
pub async fn local_copy(from: String, to_dir: String) -> Result<String, String> {
    let src = std::path::Path::new(&from);
    if !src.exists() {
        return Err(format!("来源不存在: {from}"));
    }
    let name = src
        .file_name()
        .ok_or_else(|| format!("无效来源路径: {from}"))?;
    let dst = std::path::Path::new(&to_dir).join(name);
    if dst.exists() {
        return Err(format!("目标已存在: {}", dst.display()));
    }
    if src.is_dir() {
        copy_dir_recursive(src, &dst)?;
    } else {
        std::fs::copy(src, &dst).map_err(|e| io_humanize(&e))?;
    }
    Ok(dst.to_string_lossy().replace('\\', "/"))
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| io_humanize(&e))?;
    for e in std::fs::read_dir(src).map_err(|e| io_humanize(&e))? {
        let e = e.map_err(|e| io_humanize(&e))?;
        let p = e.path();
        let meta = e.metadata().map_err(|e| io_humanize(&e))?;
        if meta.is_dir() {
            copy_dir_recursive(&p, &dst.join(e.file_name()))?;
        } else if meta.is_file() {
            std::fs::copy(&p, dst.join(e.file_name())).map_err(|e| io_humanize(&e))?;
        }
        // 软链接/其他：跳过（与 SFTP 下载同策略，不盲目跟随）
    }
    Ok(())
}

/// 桌面路径（SFTP 本地栏快捷位「桌面」）。
/// 不新增目录 crate：Windows 用 %USERPROFILE%\Desktop，类 Unix 用 $HOME/Desktop，校验存在。
#[tauri::command]
pub async fn local_desktop_path() -> Result<String, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or_else(|| "无法定位用户目录（USERPROFILE/HOME 均未设置）".to_string())?;
    let desktop = std::path::PathBuf::from(home).join("Desktop");
    if !desktop.is_dir() {
        return Err(format!("桌面目录不存在: {}", desktop.display()));
    }
    Ok(desktop.to_string_lossy().replace('\\', "/"))
}

/// 在资源管理器中定位：文件 → /select 高亮；目录 → 直接打开。
/// 注意 explorer 的退出码语义非常规（选中文件时常返回非零），spawn 成功即视为成功。
#[tauri::command]
pub async fn open_in_explorer(path: String) -> Result<(), String> {
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(format!("路径不存在: {path}"));
    }
    let mut cmd = std::process::Command::new("explorer");
    if p.is_dir() {
        cmd.arg(&path);
    } else {
        cmd.arg(format!("/select,{path}"));
    }
    cmd.spawn()
        .map_err(|e| format!("打开资源管理器失败: {e}"))?;
    Ok(())
}
