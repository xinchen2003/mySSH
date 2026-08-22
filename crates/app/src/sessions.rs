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

/// sessionId → TermOpenSpec（秘密材料从保险库取出即用；Zeroizing 在 core-ssh 边界生效）
pub async fn resolve_session_spec(
    store: &Store,
    session_id: &str,
) -> Result<crate::terminal::TermOpenSpec, String> {
    let rec = store
        .sessions()
        .get(session_id)
        .await
        .map_err(|e| e.to_string())?;
    let auth = resolve_auth(store, &rec).await?;
    Ok(crate::terminal::TermOpenSpec {
        host: rec.host.clone(),
        port: rec.port,
        user: rec.user.clone(),
        auth,
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
