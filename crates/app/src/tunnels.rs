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

#[tauri::command]
pub async fn tunnel_start(
    spec: TunnelSpecWire,
    state: tauri::State<'_, Arc<TunnelManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    // 档案存在性前置校验（错误信息比连接失败友好）
    sessions
        .store
        .sessions()
        .get(&spec.session_id)
        .await
        .map_err(|e| e.to_string())?;

    let kind = match spec.kind.as_str() {
        "local" => TunnelKind::Local {
            bind: (spec.bind_host.clone(), spec.bind_port),
        },
        "remote" => TunnelKind::Remote {
            bind: (spec.bind_host.clone(), spec.bind_port),
        },
        "dynamic" => TunnelKind::DynamicSocks5 {
            bind: (spec.bind_host.clone(), spec.bind_port),
        },
        other => return Err(format!("未知隧道类型 {other}（local|remote|dynamic）")),
    };
    let target = match (spec.target_host, spec.target_port) {
        (Some(h), Some(p)) => Some((h, p)),
        _ => None,
    };
    if !matches!(kind, TunnelKind::DynamicSocks5 { .. }) && target.is_none() {
        return Err("local/remote 隧道需要 targetHost+targetPort".into());
    }

    let id = format!("tn-{}", TUNNEL_SEQ.fetch_add(1, Ordering::Relaxed));
    let connect = make_connect_fn(sessions.store.clone(), spec.session_id);
    state
        .mgr
        .start(
            id.clone(),
            TunnelSpec {
                kind,
                target,
                max_conns: 500,
                on_disconnect: if spec.fail_fast.unwrap_or(false) {
                    DisconnectPolicy::FailFast
                } else {
                    DisconnectPolicy::Queue
                },
            },
            connect,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "tunnelId": id }))
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
