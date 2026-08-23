//! 隧道命令：tunnel_start/stop/list/subscribe。
//! ConnectFn 从会话档案解析（Bulk 类连接，窗口 4MB——07 配置基线）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::Channel;

use core_ssh::{ConnClass, ConnectOptions, KeepaliveConfig};
use core_store::Store;
use core_tunnel::{DisconnectPolicy, TunnelKind, TunnelManager, TunnelSpec};

use crate::sessions::SessionManagerState;

static TUNNEL_SEQ: AtomicU64 = AtomicU64::new(1);

pub struct TunnelManagerState {
    pub mgr: Arc<TunnelManager>,
    /// 速率差分基线：tunnelId → (时刻, bytesUp, bytesDown)
    pub(crate) last: Mutex<HashMap<String, (Instant, u64, u64)>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSpecWire {
    pub session_id: String,
    pub kind: String, // local | remote | dynamic
    pub bind_host: String,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    pub fail_fast: Option<bool>,
}

fn make_connect_fn(store: Arc<Store>, session_id: String) -> core_tunnel::ConnectFn {
    Arc::new(move || {
        let store = store.clone();
        let session_id = session_id.clone();
        Box::pin(async move {
            let spec = crate::sessions::resolve_session_spec(&store, &session_id)
                .await
                .map_err(core_ssh::SshError::Internal)?;
            // 认证材料移出，host/port/user 留用；跳板链同理（KI 一律拒绝：
            // 后台流量无交互上下文，跳板也一样）
            if matches!(spec.auth, crate::terminal::AuthSpec::KeyboardInteractive)
                || spec
                    .jump_chain
                    .iter()
                    .any(|h| matches!(h.auth, crate::terminal::AuthSpec::KeyboardInteractive))
            {
                return Err(core_ssh::SshError::UnsupportedAuth(
                    "keyboard-interactive（隧道请改用密钥/agent）",
                ));
            }
            let auth = crate::terminal::auth_method_from(&spec.auth);
            connect_for_tunnel(auth, &spec).await
        })
    })
}

async fn connect_for_tunnel(
    auth: core_ssh::AuthMethod,
    spec: &crate::terminal::TermOpenSpec,
) -> Result<core_ssh::SshConnection, core_ssh::SshError> {
    // 隧道后台流量：known_hosts 严格校验但不弹窗——首连须在终端侧完成过（已信任）
    // AcceptAll 绝不可用（安全模型第 3 条）；这里用拒绝未知主机的策略
    core_ssh::SshConnection::connect(ConnectOptions {
        host: spec.host.clone(),
        port: spec.port,
        user: spec.user.clone(),
        auth,
        jump_chain: crate::terminal::jump_chain_from(&spec.jump_chain),
        class: ConnClass::Bulk,
        // 窗口=16MB（与 spike 验证配置对齐）；07 文档 4MB 基线系 50ms RTT 推算，
        // 2026-08-23 环境回归期间实测非瓶颈（见 10-risks），保守取验证值
        window_size: 16 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        host_key_check: tunnel_host_key_check(),
        ki_prompter: None,
    })
    .await
}

/// 隧道侧主机密钥策略：known_hosts 严格校验，未知/变更一律拒绝并走日志
/// （后台流量无交互上下文；用户须先经终端侧完成首连确认——与 FinalShell 一致）
fn tunnel_host_key_check() -> core_ssh::HostKeyCheck {
    core_ssh::HostKeyCheck::KnownHosts(core_ssh::KnownHostsPolicy {
        path: crate::terminal::known_hosts_path(),
        prompter: Arc::new(|_prompt| async { core_ssh::HostKeyDecision::Reject }),
    })
}

/// 隧道定义的持久化形态（与前端 TunnelForm + 标记位对齐）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelDefWire {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub bind_host: String,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub with_session: bool,
    #[serde(default)]
    pub fail_fast: Option<bool>,
    /// true = 保存后立即建立（false 仅落库，如面板开关标记位）
    #[serde(default)]
    pub start: bool,
}

/// 启动一条隧道（隧道_start / 自启 / 随会话共用）；id 由调用方给（定义 id 或运行时序号）。
/// 参数即隧道定义的全部字段，压成结构体只会多一层间接——豁免参数数。
#[allow(clippy::too_many_arguments)]
async fn start_tunnel(
    mgr: &Arc<core_tunnel::TunnelManager>,
    store: &Arc<Store>,
    id: String,
    session_id: &str,
    kind: &str,
    bind_host: &str,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
    fail_fast: bool,
) -> Result<(), String> {
    // 幂等：同 id 已在运行/重连中则跳过
    let already = mgr.list().into_iter().any(|t| {
        t.id == id
            && matches!(
                t.status,
                core_tunnel::TunnelStatus::Starting
                    | core_tunnel::TunnelStatus::Listening
                    | core_tunnel::TunnelStatus::Reconnecting
            )
    });
    if already {
        return Ok(());
    }
    // 档案存在性前置校验（错误信息比连接失败友好）
    store
        .sessions()
        .get(session_id)
        .await
        .map_err(|e| e.to_string())?;

    let kind = match kind {
        "local" => TunnelKind::Local {
            bind: (bind_host.to_string(), bind_port),
        },
        "remote" => TunnelKind::Remote {
            bind: (bind_host.to_string(), bind_port),
        },
        "dynamic" => TunnelKind::DynamicSocks5 {
            bind: (bind_host.to_string(), bind_port),
        },
        other => return Err(format!("未知隧道类型 {other}（local|remote|dynamic）")),
    };
    let target = match (target_host, target_port) {
        (Some(h), Some(p)) => Some((h, p)),
        _ => None,
    };
    if !matches!(kind, TunnelKind::DynamicSocks5 { .. }) && target.is_none() {
        return Err("local/remote 隧道需要 targetHost+targetPort".into());
    }
    let connect = make_connect_fn(store.clone(), session_id.to_string());
    mgr.start(
        id,
        TunnelSpec {
            kind,
            target,
            max_conns: 500,
            on_disconnect: if fail_fast {
                DisconnectPolicy::FailFast
            } else {
                DisconnectPolicy::Queue
            },
        },
        connect,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn tunnel_start(
    spec: TunnelSpecWire,
    state: tauri::State<'_, Arc<TunnelManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let id = format!("tn-{}", TUNNEL_SEQ.fetch_add(1, Ordering::Relaxed));
    start_tunnel(
        &state.mgr,
        &sessions.store,
        id.clone(),
        &spec.session_id,
        &spec.kind,
        &spec.bind_host,
        spec.bind_port,
        spec.target_host,
        spec.target_port,
        spec.fail_fast.unwrap_or(false),
    )
    .await?;
    Ok(json!({ "tunnelId": id }))
}

/// 保存隧道定义并立即建立（幂等）；返回 tunnelId=定义 id
#[tauri::command]
pub async fn tunnel_save(
    def: TunnelDefWire,
    state: tauri::State<'_, Arc<TunnelManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    sessions
        .store
        .tunnels()
        .upsert(&core_store::TunnelRecord {
            id: def.id.clone(),
            session_id: def.session_id.clone(),
            kind: def.kind.clone(),
            bind_host: def.bind_host.clone(),
            bind_port: def.bind_port,
            target_host: def.target_host.clone(),
            target_port: def.target_port,
            autostart: def.autostart,
            with_session: def.with_session,
            created_at: String::new(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let _ = sessions
        .store
        .audit()
        .append(
            core_store::Actor::Gui,
            Some(&def.session_id),
            "tunnel_save",
            &json!({ "tunnelId": def.id, "kind": def.kind }),
        )
        .await;
    if def.start {
        start_tunnel(
            &state.mgr,
            &sessions.store,
            def.id.clone(),
            &def.session_id,
            &def.kind,
            &def.bind_host,
            def.bind_port,
            def.target_host.clone(),
            def.target_port,
            def.fail_fast.unwrap_or(false),
        )
        .await?;
    }
    Ok(json!({ "tunnelId": def.id }))
}

/// 停止运行 + 删除定义（级联审计保留）
#[tauri::command]
pub async fn tunnel_delete(
    tunnel_id: String,
    state: tauri::State<'_, Arc<TunnelManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let _ = state.mgr.stop(&tunnel_id).await; // 未运行忽略
    sessions
        .store
        .tunnels()
        .delete(&tunnel_id)
        .await
        .map_err(|e| e.to_string())?;
    let _ = sessions
        .store
        .audit()
        .append(
            core_store::Actor::Gui,
            None,
            "tunnel_delete",
            &json!({ "tunnelId": tunnel_id }),
        )
        .await;
    Ok(())
}

/// 持久化的隧道定义列表（运行态由 tunnel_list/tunnel_subscribe 提供）
#[tauri::command]
pub async fn tunnel_defs(
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let defs = sessions
        .store
        .tunnels()
        .list()
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(defs).map_err(|e| e.to_string())
}

/// app 启动：拉起 autostart 定义（在 lib.rs setup 调用）
pub async fn autostart_tunnels(mgr: Arc<core_tunnel::TunnelManager>, store: Arc<Store>) {
    let defs = match store.tunnels().list().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "隧道定义读取失败，跳过自启");
            return;
        }
    };
    for d in defs.into_iter().filter(|d| d.autostart) {
        let r = start_tunnel(
            &mgr,
            &store,
            d.id.clone(),
            &d.session_id,
            &d.kind,
            &d.bind_host,
            d.bind_port,
            d.target_host,
            d.target_port,
            false,
        )
        .await;
        if let Err(e) = r {
            tracing::warn!(tunnel = %d.id, error = %e, "自启隧道建立失败（监督器将继续重连）");
        }
    }
}

/// 会话连接成功：拉起该会话 with_session 的定义（term_open 调用，fire-and-forget）
pub async fn start_session_tunnels(
    mgr: Arc<core_tunnel::TunnelManager>,
    store: Arc<Store>,
    session_id: String,
) {
    let defs = match store.tunnels().for_session(&session_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "随会话隧道定义读取失败");
            return;
        }
    };
    for d in defs.into_iter().filter(|d| d.with_session) {
        let r = start_tunnel(
            &mgr,
            &store,
            d.id.clone(),
            &d.session_id,
            &d.kind,
            &d.bind_host,
            d.bind_port,
            d.target_host,
            d.target_port,
            false,
        )
        .await;
        if let Err(e) = r {
            tracing::warn!(tunnel = %d.id, error = %e, "随会话隧道建立失败");
        }
    }
}

#[tauri::command]
pub async fn tunnel_stop(
    tunnel_id: String,
    state: tauri::State<'_, Arc<TunnelManagerState>>,
) -> Result<(), String> {
    state.mgr.stop(&tunnel_id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn tunnel_list(
    state: tauri::State<'_, Arc<TunnelManagerState>>,
) -> Result<Value, String> {
    let list = state.mgr.list();
    Ok(json!(list
        .iter()
        .map(|t| info_to_json(state.inner(), t))
        .collect::<Vec<_>>()))
}

/// 1Hz 聚合推送隧道状态（规格书第 9 条）；Channel 关闭即停
#[tauri::command]
pub async fn tunnel_subscribe(
    events: Channel<Value>,
    state: tauri::State<'_, Arc<TunnelManagerState>>,
) -> Result<(), String> {
    let mgr = state.mgr.clone();
    let st = state.inner().clone();
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let frames: Vec<Value> = mgr.list().iter().map(|t| info_to_json(&st, t)).collect();
            if events
                .send(json!({ "v": 1, "type": "tunnel_stats", "tunnels": frames }))
                .is_err()
            {
                break; // 前端 Channel 已弃
            }
        }
    });
    Ok(())
}

fn info_to_json(st: &TunnelManagerState, t: &core_tunnel::TunnelInfo) -> Value {
    let now = Instant::now();
    let (rate_up, rate_down) = {
        let mut last = st.last.lock();
        let prev = last.insert(t.id.clone(), (now, t.stats.bytes_up, t.stats.bytes_down));
        match prev {
            Some((t0, up0, down0)) => {
                let dt = now.duration_since(t0).as_secs_f64().max(0.001);
                (
                    (t.stats.bytes_up.saturating_sub(up0) as f64 / dt) as u64,
                    (t.stats.bytes_down.saturating_sub(down0) as f64 / dt) as u64,
                )
            }
            None => (0, 0),
        }
    };
    json!({
        "tunnelId": t.id,
        "kind": t.kind,
        "bind": t.bind,
        "target": t.target,
        "status": match t.status {
            core_tunnel::TunnelStatus::Starting => "starting",
            core_tunnel::TunnelStatus::Listening => "listening",
            core_tunnel::TunnelStatus::Reconnecting => "reconnecting",
            core_tunnel::TunnelStatus::Stopped => "stopped",
            core_tunnel::TunnelStatus::Failed => "failed",
        },
        "activeConns": t.stats.active_conns,
        "totalConns": t.stats.total_conns,
        "bytesUp": t.stats.bytes_up,
        "bytesDown": t.stats.bytes_down,
        "rateUp": rate_up,
        "rateDown": rate_down,
        "errors": t.stats.errors,
        "reconnects": t.stats.reconnects,
    })
}
