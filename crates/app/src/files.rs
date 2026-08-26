//! 受限文件读取：连接对话框选择私钥文件后读入内存（不持久化，Zeroizing 由 core-ssh 负责）。

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
/// 拖放导入·创建目标文件，返回绝对路径（HTML5 拖放的 File 对象无完整路径，
/// 前端只能读字节流——先落盘，再走既有 sftp_upload 队列，进度/续传/审计全复用）。
/// - dest_dir=None → %TEMP%/myssh-drops/<唯一目录>/ 中转区（上传用）
/// - dest_dir=Some → 直接写入该目录（OS 拖入本地栏 = 复制进当前目录）
/// rel_path 可含子目录（文件夹拖入保结构），父目录自动创建；目标已存在 → 拒绝（不静默覆盖）。
#[tauri::command]
pub async fn local_drop_begin(
    dest_dir: Option<String>,
    rel_path: String,
) -> Result<String, String> {
    let rel = std::path::Path::new(&rel_path);
    // 防逃逸：拒绝绝对路径与 .. 分量
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("非法相对路径: {rel_path}"));
    }
    let base = match &dest_dir {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            let root = std::env::temp_dir().join("myssh-drops");
            sweep_drops(&root);
            let millis = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let dir = root.join(format!("{}-{}", millis, std::process::id()));
            std::fs::create_dir_all(&dir).map_err(|e| format!("创建中转目录失败: {e}"))?;
            dir
        }
    };
    let dst = base.join(rel);
    if dst.exists() {
        return Err(format!("目标已存在: {}", dst.display()));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    std::fs::File::create(&dst).map_err(|e| io_humanize(&e))?;
    Ok(dst.to_string_lossy().into_owned())
}

/// 拖放导入·追加一块数据（base64；前端按 4MB 分块，避免大文件整体进内存/单条 IPC 过大）
#[tauri::command]
pub async fn local_drop_append(path: String, data_b64: String) -> Result<(), String> {
    use base64::Engine as _;
    use std::io::Write as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| format!("base64 解码失败: {e}"))?;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .map_err(|e| io_humanize(&e))?;
    f.write_all(&bytes).map_err(|e| io_humanize(&e))
}

/// 清理 myssh-drops 中转区 24h 前的残留（上传中断/进程被杀留下的半成品）
fn sweep_drops(root: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(24 * 3600);
    for e in rd.flatten() {
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
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
