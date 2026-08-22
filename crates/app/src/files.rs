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
