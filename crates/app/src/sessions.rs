//! 会话档案与凭据命令：CRUD + term_open 的 sessionId 解析。
//! 秘密材料只在内存经手：cred_set 直接进保险库，term_open 解析时读出即用。

use std::sync::Arc;

use serde_json::{json, Value};

use core_ssh::{
    ConnClass, ConnectOptions, HostKeyCheck, HostKeyDecision, HostKeyPrompt, KeepaliveConfig,
    KnownHostsPolicy, SshConnection,
};
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
    tunnels_state: tauri::State<'_, Arc<crate::tunnels::TunnelManagerState>>,
) -> Result<(), String> {
    // 先停运行中隧道（定义还在库中可查），再删会话（FK 级联删定义）
    crate::tunnels::stop_all_session_tunnels(
        tunnels_state.mgr.clone(),
        state.store.clone(),
        session_id.clone(),
    )
    .await;
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
/// 分组重命名/移动（含子树；事务性前缀改写；目标在源子树内拒绝）
#[tauri::command]
pub async fn group_rename(
    old_path: String,
    new_path: String,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let affected = state
        .store
        .sessions()
        .group_rename(&old_path, &new_path)
        .await
        .map_err(|e| e.to_string())?;
    state
        .store
        .audit()
        .append(
            Actor::Gui,
            None,
            "group_rename",
            &json!({ "oldPath": old_path, "newPath": new_path, "affected": affected }),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "affected": affected }))
}

/// 分组删除：with_sessions=false 保留会话（直属移未分组、子分组上移父级）；
/// true 删除子树全部会话（凭据与隧道由 FK 级联）
#[tauri::command]
pub async fn group_delete(
    path: String,
    with_sessions: bool,
    state: tauri::State<'_, Arc<SessionManagerState>>,
    tunnels_state: tauri::State<'_, Arc<crate::tunnels::TunnelManagerState>>,
) -> Result<Value, String> {
    // 级联删除前收集受影响会话，删库后停止其运行中隧道（理由同 session_delete）
    let doomed: Vec<String> = if with_sessions {
        state
            .store
            .sessions()
            .list()
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|s| s.group_path == path || s.group_path.starts_with(&format!("{path}/")))
            .map(|s| s.id)
            .collect()
    } else {
        Vec::new()
    };
    for sid in doomed {
        crate::tunnels::stop_all_session_tunnels(
            tunnels_state.mgr.clone(),
            state.store.clone(),
            sid,
        )
        .await;
    }
    let affected = state
        .store
        .sessions()
        .group_delete(&path, with_sessions)
        .await
        .map_err(|e| e.to_string())?;
    state
        .store
        .audit()
        .append(
            Actor::Gui,
            None,
            "group_delete",
            &json!({ "path": path, "withSessions": with_sessions, "affected": affected }),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "affected": affected }))
}

/// 批量移动会话到分组（'' = 未分组）
#[tauri::command]
pub async fn session_move(
    session_ids: Vec<String>,
    group_path: String,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let moved = state
        .store
        .sessions()
        .move_to_group(&session_ids, &group_path)
        .await
        .map_err(|e| e.to_string())?;
    state
        .store
        .audit()
        .append(
            Actor::Gui,
            None,
            "session_move",
            &json!({ "count": session_ids.len(), "groupPath": group_path, "moved": moved }),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "moved": moved }))
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

/// 连通性测试入参（秘密材料只在内存经手；authType 与 SessionRecord 同 serde 形式）
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestConnectRequest {
    /// 关联会话（保险库回退 + 审计归属）；可空 = 表单草稿未保存
    pub session_id: Option<String>,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth_type: AuthType,
    pub password: Option<String>,
    pub key_path: Option<String>,
    pub passphrase: Option<String>,
    /// 跳板会话 id 链（就近→最远）；空 = 直连
    #[serde(default)]
    pub jump_chain: Vec<String>,
}

/// 连通性测试：8s 超时；永远返回 Ok（ok:false + error 表失败）
#[tauri::command]
pub async fn session_test_connect(
    req: TestConnectRequest,
    state: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        test_connect_inner(&state.store, &req),
    )
    .await;
    let body = match outcome {
        Ok(Ok(latency_ms)) => json!({ "ok": true, "latencyMs": latency_ms }),
        Ok(Err(e)) => json!({ "ok": false, "error": e }),
        Err(_) => json!({ "ok": false, "error": "连接超时（8 秒）" }),
    };
    // 审计失败不影响测试结果回传
    let _ = state
        .store
        .audit()
        .append(
            Actor::Gui,
            req.session_id.as_deref(),
            "session_test_connect",
            &json!({ "host": req.host, "port": req.port, "ok": body["ok"] }),
        )
        .await;
    Ok(body)
}

/// 建连 → 握手完成即测延迟并立即关闭；返回毫秒数
async fn test_connect_inner(store: &Store, req: &TestConnectRequest) -> Result<u64, String> {
    let auth = resolve_test_auth(store, req).await?;
    // 跳板链：复用 resolve_spec_inner 的递归展开（含环/深度防护）
    let mut visited = std::collections::HashSet::new();
    let mut jump_chain = Vec::new();
    for hop_id in &req.jump_chain {
        let hop = Box::pin(resolve_spec_inner(store, hop_id, &mut visited)).await?;
        jump_chain.extend(hop.jump_chain);
        jump_chain.push(crate::terminal::JumpHopSpec {
            host: hop.host,
            port: hop.port,
            user: hop.user,
            auth: hop.auth,
        });
    }
    let opts = ConnectOptions {
        host: req.host.clone(),
        port: req.port,
        user: req.user.clone(),
        auth: crate::terminal::auth_method_from(&auth),
        jump_chain: crate::terminal::jump_chain_from(&jump_chain),
        class: ConnClass::Bulk,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        // 测试连接无弹窗通路：首连/变更一律 fail-closed 拒绝（经正式连接学入 known_hosts）
        host_key_check: HostKeyCheck::KnownHosts(KnownHostsPolicy {
            path: crate::terminal::known_hosts_path(),
            prompter: Arc::new(|_: HostKeyPrompt| async { HostKeyDecision::Reject }),
        }),
        ki_prompter: None,
    };
    let started = std::time::Instant::now();
    let conn = SshConnection::connect(opts)
        .await
        .map_err(|e| e.to_string())?;
    let latency_ms = started.elapsed().as_millis() as u64;
    drop(conn); // 测完即关（句柄 drop 即断连）
    Ok(latency_ms)
}

/// 测试连接认证：payload 秘密优先；为空且带 sessionId 时回退保险库
async fn resolve_test_auth(store: &Store, req: &TestConnectRequest) -> Result<AuthSpec, String> {
    match req.auth_type {
        AuthType::Password => {
            let password = match req.password.as_deref().filter(|p| !p.is_empty()) {
                Some(p) => p.to_string(),
                None => {
                    let sid = req.session_id.as_deref().ok_or("未提供密码且未关联会话")?;
                    let secret = store
                        .credentials()
                        .get(sid)
                        .await
                        .map_err(|_| format!("会话 {sid} 未存密码（cred_set）"))?;
                    String::from_utf8(secret.expose().to_vec())
                        .map_err(|_| "凭据非 UTF-8".to_string())?
                }
            };
            Ok(AuthSpec::Password { password })
        }
        AuthType::PublicKey => {
            let key_path = match req.key_path.as_deref().filter(|p| !p.is_empty()) {
                Some(p) => p.to_string(),
                None => {
                    let sid = req
                        .session_id
                        .as_deref()
                        .ok_or("未提供 keyPath 且未关联会话")?;
                    store
                        .sessions()
                        .get(sid)
                        .await
                        .map_err(|e| e.to_string())?
                        .key_path
                        .ok_or_else(|| format!("会话 {sid} 未配 keyPath"))?
                }
            };
            let key_pem = read_key_file(&key_path)?;
            let passphrase = match req.passphrase.as_deref().filter(|p| !p.is_empty()) {
                Some(p) => Some(p.to_string()),
                None => match &req.session_id {
                    // passphrase 可选：保险库里没有就当无
                    Some(sid) => match store.credentials().get(sid).await {
                        Ok(s) => Some(
                            String::from_utf8(s.expose().to_vec())
                                .map_err(|_| "凭据非 UTF-8".to_string())?,
                        ),
                        Err(_) => None,
                    },
                    None => None,
                },
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
/// term_open 的解析结果：SSH 连接参数 或 本地 PTY 参数（批次十四 本地会话）
pub enum ResolvedTarget {
    Ssh(crate::terminal::TermOpenSpec),
    Local(crate::local_pty::LocalShellSpec),
}

/// sessionId → ResolvedTarget：local 会话在此分流（不触碰凭据/跳板解析）。
/// SSH 专属调用方（sftp/monitor/tunnel）继续用 resolve_session_spec——本地会话在那里报错。
pub async fn resolve_session_target(
    store: &Store,
    session_id: &str,
) -> Result<ResolvedTarget, String> {
    let rec = store
        .sessions()
        .get(session_id)
        .await
        .map_err(|e| e.to_string())?;
    if rec.kind == core_store::SessionKind::Local {
        return Ok(ResolvedTarget::Local(crate::local_pty::LocalShellSpec {
            shell: rec.shell.clone(),
            workdir: rec.workdir.clone(),
            command: rec.command.clone(),
        }));
    }
    let mut visited = std::collections::HashSet::new();
    Ok(ResolvedTarget::Ssh(
        resolve_spec_inner(store, session_id, &mut visited).await?,
    ))
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
    if rec.kind == core_store::SessionKind::Local {
        return Err(format!("会话 {} 是本地终端，不支持 SSH 连接", rec.name));
    }
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
        encoding: rec.encoding.clone(),
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
