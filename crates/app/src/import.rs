//! ssh_config 批量导入命令：预览（解析 + 重名标注）与勾选落库。
//! 解析器在 core-store（纯函数）；此处负责默认路径、重名检测、跳板名解析与审计。

use std::collections::HashMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};

use core_store::{Actor, AuthType, SessionKind, SessionRecord, SshConfigEntry};

use crate::sessions::SessionManagerState;

/// 预览条目 = 解析结果 + 与现有会话的重名标注
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreviewEntry {
    #[serde(flatten)]
    entry: SshConfigEntry,
    conflict: bool,
}

/// 导入结果；warnings 为逐条警告详情（跳板解析不到 / 单条写入失败）
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ImportOutcome {
    imported: u32,
    skipped: u32,
    warnings: Vec<String>,
}

/// 用户主目录：Windows 取 %USERPROFILE%，其它平台回落 $HOME
fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
}

fn default_ssh_config_path() -> std::path::PathBuf {
    home_dir()
        .map(|h| h.join(".ssh").join("config"))
        .unwrap_or_else(|| std::path::PathBuf::from("~/.ssh/config"))
}

/// 预览：解析 ssh_config（path 缺省 = 用户目录下 .ssh/config），标注与现有会话的重名
#[tauri::command]
pub async fn ssh_config_preview(
    path: Option<String>,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let file = path
        .filter(|p| !p.trim().is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(default_ssh_config_path);
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("未找到 ssh_config 文件：{}", file.display()));
        }
        Err(e) => {
            return Err(format!("读取 ssh_config 失败：{}（{e}）", file.display()));
        }
    };
    let home = home_dir().map(|p| p.to_string_lossy().into_owned());
    let entries = core_store::parse_ssh_config(&text, home.as_deref());
    let existing = state
        .store
        .sessions()
        .list()
        .await
        .map_err(|e| e.to_string())?;
    let out: Vec<PreviewEntry> = entries
        .into_iter()
        .map(|entry| PreviewEntry {
            conflict: entry.skipped.is_none() && existing.iter().any(|s| s.name == entry.alias),
            entry,
        })
        .collect();
    serde_json::to_value(out).map_err(|e| e.to_string())
}

/// 批量导入勾选的条目：逐条建会话（kind=ssh，name=alias）。
/// ProxyJump 按会话名解析：本批导入条目优先，回落现有会话；解析不到置空并计入 warnings。
#[tauri::command]
pub async fn ssh_config_import(
    entries: Vec<SshConfigEntry>,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let store = &state.store;
    let existing = store.sessions().list().await.map_err(|e| e.to_string())?;
    // 解析器已跳过（skipped 有原因）的条目防御性剔除，计入 skipped
    let skipped = entries.iter().filter(|e| e.skipped.is_some()).count() as u32;
    let batch: Vec<&SshConfigEntry> = entries.iter().filter(|e| e.skipped.is_none()).collect();

    // 名称 → 会话 id：先放本批条目（预生成 id，同批内先出现者优先），再以现有会话补空位
    let mut name_to_id: HashMap<String, String> = HashMap::new();
    let batch_ids: Vec<String> = batch
        .iter()
        .map(|e| {
            let id = new_session_id();
            name_to_id
                .entry(e.alias.clone())
                .or_insert_with(|| id.clone());
            id
        })
        .collect();
    for rec in &existing {
        name_to_id
            .entry(rec.name.clone())
            .or_insert_with(|| rec.id.clone());
    }

    let mut imported = 0u32;
    let mut warnings: Vec<String> = Vec::new();
    let mut write_failed = 0u32;
    for (e, id) in batch.iter().zip(batch_ids) {
        let jump_chain = match &e.proxy_jump {
            Some(j) => match name_to_id.get(j) {
                Some(target) => vec![target.clone()],
                None => {
                    warnings.push(format!(
                        "{}：跳板 {j} 未找到（不在本次导入或现有会话中），已置空",
                        e.alias
                    ));
                    Vec::new()
                }
            },
            None => Vec::new(),
        };
        // 有 IdentityFile → 公钥认证指向该路径；无 → 密码认证留空待补（密码入保险库由用户后续设置）
        let rec = SessionRecord {
            id,
            name: e.alias.clone(),
            kind: SessionKind::Ssh,
            host: e.hostname.clone(),
            port: e.port,
            user: e.user.clone(),
            auth_type: if e.identity_file.is_some() {
                AuthType::PublicKey
            } else {
                AuthType::Password
            },
            key_path: e.identity_file.clone(),
            shell: None,
            workdir: None,
            jump_chain,
            group_path: String::new(),
            color: None,
            encoding: "utf-8".into(),
            su_user: None,
            login_macro: None,
            mcp_perms: std::collections::HashMap::new(),
            tags: Vec::new(),
            command: None,
            // upsert 的 INSERT 不绑定 created_at（DB 默认 datetime('now')）
            created_at: String::new(),
            updated_at: String::new(),
        };
        match store.sessions().upsert(&rec).await {
            Ok(_) => imported += 1,
            Err(err) => {
                write_failed += 1;
                warnings.push(format!("{}：写入失败: {err}", e.alias));
            }
        }
    }

    store
        .audit()
        .append(
            Actor::Gui,
            None,
            "ssh_config_import",
            &json!({
                "imported": imported,
                "skipped": skipped + write_failed,
                "warnings": warnings,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(ImportOutcome {
        imported,
        skipped: skipped + write_failed,
        warnings,
    })
    .map_err(|e| e.to_string())
}

/// 会话 id：16 字节随机 hex（仿 mcp.rs gen_token；与前端 crypto.randomUUID 同为不透明字符串）
fn new_session_id() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
