//! 会话档案与凭据命令：CRUD + term_open 的 sessionId 解析。
//! 秘密材料只在内存经手：cred_set 直接进保险库，term_open 解析时读出即用。

use std::sync::Arc;

use serde_json::{json, Value};

use core_store::{Actor, AuthType, CredentialKind, Secret, SessionRecord, Store};

use crate::terminal::AuthSpec;

pub struct SessionManagerState {
    pub store: Arc<Store>,
}

pub fn store_path() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("myssh")
        .join("myssh.db")
}

#[tauri::command]
pub async fn session_list(
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let list = state
        .store
        .sessions()
        .list()
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(list).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn session_upsert(
    record: SessionRecord,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let rec = state
        .store
        .sessions()
        .upsert(&record)
        .await
        .map_err(|e| e.to_string())?;
    state
        .store
        .audit()
        .append(
            Actor::Gui,
            Some(&rec.id),
            "session_upsert",
            &json!({ "name": rec.name, "host": rec.host }),
        )
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(rec).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn session_delete(
    session_id: String,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    state
        .store
        .sessions()
        .delete(&session_id)
        .await
        .map_err(|e| e.to_string())?;
    state
        .store
        .audit()
        .append(Actor::Gui, Some(&session_id), "session_delete", &json!({}))
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 秘密直进保险库（密码或私钥 passphrase）；不回读、不回显
#[tauri::command]
pub async fn cred_set(
    session_id: String,
    kind: CredentialKindSpec,
    secret: String,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let kind = match kind {
        CredentialKindSpec::Password => CredentialKind::Password,
        CredentialKindSpec::KeyPassphrase => CredentialKind::KeyPassphrase,
    };
    state
        .store
        .credentials()
        .put(&session_id, kind, &Secret::new(secret.into_bytes()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cred_delete(
    session_id: String,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    state
        .store
        .credentials()
        .delete(&session_id)
        .await
        .map_err(|e| e.to_string())
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CredentialKindSpec {
    Password,
    KeyPassphrase,
}

#[tauri::command]
pub async fn vault_status(
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let status = state.store.credentials().status();
    Ok(json!({ "unlocked": status == core_store::VaultStatus::Unlocked }))
}

/// 导入 OpenSSH 客户端配置（缺省 ~/.ssh/config）；幂等
#[tauri::command]
pub async fn import_openssh(
    path: Option<String>,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "无法定位用户主目录".to_string())?;
    let file = path
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join(".ssh").join("config"));
    let text =
        std::fs::read_to_string(&file).map_err(|e| format!("读取 {} 失败: {e}", file.display()))?;
    let home_str = home.to_string_lossy().to_string();
    let outcome = core_store::import_openssh(&state.store, &text, &home_str)
        .await
        .map_err(|e| e.to_string())?;
    state
        .store
        .audit()
        .append(
            Actor::Gui,
            None,
            "import_openssh",
            &json!({ "path": file.display().to_string(), "imported": outcome.imported, "skipped": outcome.skipped }),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "imported": outcome.imported,
        "skipped": outcome.skipped,
        "unresolvedJumps": outcome.unresolved_jumps,
    }))
}

/// 配置导出：写 %LOCALAPPDATA%/myssh/exports/myssh-config-<ts>.json，返回路径。
/// encrypted=true 时需 passphrase（Argon2id 派生密钥，含凭据）；false = 明文（绝不含秘密）。
#[tauri::command]
pub async fn config_export(
    encrypted: bool,
    passphrase: Option<String>,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    if encrypted && passphrase.as_deref().unwrap_or("").is_empty() {
        return Err("加密导出需要导出口令".into());
    }
    let text = if encrypted {
        core_store::export_encrypted(&state.store, passphrase.as_deref().unwrap_or("").as_bytes())
            .await
    } else {
        core_store::export_plain(&state.store).await
    }
    .map_err(|e| e.to_string())?;
    let dir = store_path()
        .parent()
        .map(|p| p.join("exports"))
        .ok_or_else(|| "无法定位导出目录".to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导出目录失败: {e}"))?;
    let file = dir.join(format!(
        "myssh-config-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    ));
    std::fs::write(&file, text).map_err(|e| format!("写导出文件失败: {e}"))?;
    state
        .store
        .audit()
        .append(
            Actor::Gui,
            None,
            "config_export",
            &json!({ "path": file.display().to_string(), "encrypted": encrypted }),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "path": file.display().to_string() }))
}

/// 配置导入（自动识别明文/加密包络；加密需口令）
#[tauri::command]
pub async fn config_import(
    path: String,
    passphrase: Option<String>,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let text = std::fs::read_to_string(&path).map_err(|e| format!("读取 {path} 失败: {e}"))?;
    let outcome = core_store::import_config(
        &state.store,
        &text,
        passphrase.as_deref().map(str::as_bytes),
    )
    .await
    .map_err(|e| e.to_string())?;
    state
        .store
        .audit()
        .append(
            Actor::Gui,
            None,
            "config_import",
            &json!({ "path": path, "sessions": outcome.sessions, "tunnels": outcome.tunnels, "credentials": outcome.credentials }),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({
        "sessions": outcome.sessions,
        "tunnels": outcome.tunnels,
        "credentials": outcome.credentials,
    }))
}

/// sessionId → TermOpenSpec（秘密材料从保险库取出即用；Zeroizing 在 core-ssh 边界生效）
pub async fn resolve_session_spec(
    store: &Store,
    session_id: &str,
) -> Result<crate::terminal::TermOpenSpec, String> {
    let mut visited = std::collections::HashSet::new();
    resolve_spec_inner(store, session_id, &mut visited).await
}

/// 单跳深度上限：防环兜底（visited 集已严格防环，此为异常防御）
const MAX_JUMP_DEPTH: usize = 8;

async fn resolve_spec_inner(
    store: &Store,
    session_id: &str,
    visited: &mut std::collections::HashSet<String>,
) -> Result<crate::terminal::TermOpenSpec, String> {
    if !visited.insert(session_id.to_string()) {
        return Err(format!("跳板链存在环：{session_id} 重复出现"));
    }
    if visited.len() > MAX_JUMP_DEPTH {
        return Err(format!("跳板链超过 {MAX_JUMP_DEPTH} 跳上限"));
    }
    let rec = store
        .sessions()
        .get(session_id)
        .await
        .map_err(|e| e.to_string())?;
    let auth = resolve_auth(store, &rec).await?;
    // 逐跳解析（就近→最远）；跳板的跳板递归展开拍平——
    // 若跳 A 自身配了跳板 B，则链为 B..A（B 更靠近本机）
    let mut jump_chain = Vec::new();
    for hop_id in &rec.jump_chain {
        let hop = Box::pin(resolve_spec_inner(store, hop_id, visited)).await?;
        jump_chain.extend(hop.jump_chain);
        jump_chain.push(crate::terminal::JumpHopSpec {
            host: hop.host,
            port: hop.port,
            user: hop.user,
            auth: hop.auth,
        });
    }
    Ok(crate::terminal::TermOpenSpec {
        host: rec.host.clone(),
        port: rec.port,
        user: rec.user.clone(),
        auth,
        jump_chain,
        term: None,
        command: rec.command.clone(),
    })
}

async fn resolve_auth(store: &Store, rec: &SessionRecord) -> Result<AuthSpec, String> {
    match rec.auth_type {
        AuthType::Password => {
            let secret = store
                .credentials()
                .get(&rec.id)
                .await
                .map_err(|_| format!("会话 {} 未存密码（cred_set）", rec.id))?;
            let text = String::from_utf8(secret.expose().to_vec())
                .map_err(|_| "凭据非 UTF-8".to_string())?;
            Ok(AuthSpec::Password { password: text })
        }
        AuthType::PublicKey => {
            let key_path = rec
                .key_path
                .as_deref()
                .ok_or_else(|| format!("会话 {} 未配 keyPath", rec.id))?;
            let key_pem = read_key_file(key_path)?;
            // passphrase 可选：保险库里没有就当无
            let passphrase = match store.credentials().get(&rec.id).await {
                Ok(s) => Some(
                    String::from_utf8(s.expose().to_vec())
                        .map_err(|_| "凭据非 UTF-8".to_string())?,
                ),
                Err(_) => None,
            };
            Ok(AuthSpec::PublicKey {
                key_pem,
                passphrase,
            })
        }
        AuthType::KeyboardInteractive => Ok(AuthSpec::KeyboardInteractive),
        AuthType::Agent => Ok(AuthSpec::Agent),
    }
}

fn read_key_file(path: &str) -> Result<String, String> {
    let p = std::path::Path::new(path);
    std::fs::read_to_string(p).map_err(|e| format!("私钥读取失败 {path}: {e}"))
}
