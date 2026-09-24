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

use core_store::{Store, StoreError};
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
            // 后台建连策略收口于 connect 模块（卡 5）：KI 拒绝/hostkey 严格/Bulk/16MB 档
            crate::connect::background(&spec, crate::connect::ConnectProfile::Throughput).await
        })
    })
}

/// 共享键（PR-15）：sessionId + 结构化配置指纹（host/port/user + 认证方式判别式
/// + jump 链逐跳拓扑；秘密永不入指纹——凭据轮换不拆组，重连经 resolve 取新凭据）。
///   Session 配置变更 → 指纹变 → 重启后新隧道进新组，旧组随 drain 与 lease 归零关闭。
async fn tunnel_group_key(store: &Arc<Store>, session_id: &str) -> Result<String, String> {
    let spec = crate::sessions::resolve_session_spec(store, session_id).await?;
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    spec.host.hash(&mut h);
    spec.port.hash(&mut h);
    spec.user.hash(&mut h);
    crate::connect::hash_auth(&mut h, &spec.auth);
    for hop in &spec.jump_chain {
        hop.host.hash(&mut h);
        hop.port.hash(&mut h);
        hop.user.hash(&mut h);
        crate::connect::hash_auth(&mut h, &hop.auth);
    }
    Ok(format!("{session_id}:{:016x}", h.finish()))
}

/// 隧道定义的持久化形态（与前端 TunnelForm + 标记位对齐）
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelDefWire {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    #[serde(default)]
    pub name: String,
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
    let group_key = tunnel_group_key(store, session_id).await?;
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
            stop_grace_timeout: core_tunnel::DEFAULT_STOP_GRACE_TIMEOUT,
            half_close_drain_timeout: core_tunnel::DEFAULT_HALF_CLOSE_DRAIN_TIMEOUT,
            session_id: session_id.to_string(),
        },
        group_key,
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
            name: def.name.clone(),
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
    events: Channel<Value>,
) {
    let defs = match store.tunnels().for_session(&session_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "随会话隧道定义读取失败");
            return;
        }
    };
    let mut results: Vec<crate::wire::SessionTunnelResult> = Vec::new();
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
        if let Err(e) = &r {
            tracing::warn!(tunnel = %d.id, error = %e, "随会话隧道建立失败");
        }
        // 幂等跳过（已在运行）视为成功——重连场景下重复拉起不算失败
        let already = mgr.list().into_iter().any(|t| {
            t.id == d.id
                && matches!(
                    t.status,
                    core_tunnel::TunnelStatus::Starting
                        | core_tunnel::TunnelStatus::Listening
                        | core_tunnel::TunnelStatus::Reconnecting
                )
        });
        results.push(crate::wire::SessionTunnelResult {
            id: d.id.clone(),
            name: d.name.clone(),
            bind: format!("{}:{}", d.bind_host, d.bind_port),
            ok: r.is_ok() || already,
            error: if already { None } else { r.err() },
        });
    }
    if !results.is_empty() {
        let _ = events.send(crate::wire::json_of(
            &crate::wire::SessionTunnelsFrame::new(&session_id, results),
        ));
    }
}
/// 会话断开（终端关闭/重连耗尽）：停止该会话 with_session 的运行中隧道。
/// 仅当同会话无其他存活终端标签时调用（调用方负责判定）。
pub async fn stop_session_tunnels(
    mgr: Arc<core_tunnel::TunnelManager>,
    store: Arc<Store>,
    session_id: String,
) {
    let defs = match store.tunnels().for_session(&session_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "随会话隧道定义读取失败（停止侧）");
            return;
        }
    };
    for d in defs.into_iter().filter(|d| d.with_session) {
        match mgr.stop(&d.id).await {
            Ok(()) | Err(core_tunnel::TunnelError::NotFound(_)) => {}
            Err(e) => tracing::warn!(tunnel = %d.id, error = %e, "随会话隧道停止失败"),
        }
    }
}

/// Session 配置变更（PR-15 评审定稿：旧组 draining、新连接进新组）：
/// 重启该会话全部在跑隧道——stop 按 grace 让旧 relay 自然收尾（超时强制），
/// 旧组随 lease 归零关闭；新启动按新配置指纹建/入新组。
pub async fn restart_session_tunnels(
    mgr: Arc<core_tunnel::TunnelManager>,
    store: Arc<Store>,
    session_id: &str,
) {
    let defs = match store.tunnels().for_session(session_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(error = %e, "隧道定义读取失败（配置变更重启跳过）");
            return;
        }
    };
    let running: std::collections::HashSet<String> = mgr
        .list()
        .into_iter()
        .filter(|t| {
            matches!(
                t.status,
                core_tunnel::TunnelStatus::Starting
                    | core_tunnel::TunnelStatus::Listening
                    | core_tunnel::TunnelStatus::Reconnecting
            )
        })
        .map(|t| t.id)
        .collect();
    for d in defs.into_iter().filter(|d| running.contains(&d.id)) {
        match mgr.stop(&d.id).await {
            Ok(()) | Err(core_tunnel::TunnelError::NotFound(_)) => {}
            Err(e) => {
                tracing::warn!(tunnel = %d.id, error = %e, "配置变更停止隧道失败");
                continue;
            }
        }
        if let Err(e) = start_tunnel(
            &mgr,
            &store,
            d.id.clone(),
            session_id,
            &d.kind,
            &d.bind_host,
            d.bind_port,
            d.target_host.clone(),
            d.target_port,
            false,
        )
        .await
        {
            tracing::warn!(tunnel = %d.id, error = %e, "配置变更后重启隧道失败");
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
    crate::wire::json_of(&crate::wire::TunnelView::from_info(t, rate_up, rate_down))
}

/// 本地端口预检（§9.4）：创建/编辑本地或动态隧道前调用。
/// - 占用者为本隧道管理器中的其他定义 → 报占用来源；为 exclude_tunnel_id 自身 → 视为可用；
/// - 占用时为可用建议端口（向上探测至多 100 个）；绝不静默修改用户端口。
#[tauri::command]
pub async fn tunnel_check_port(
    host: String,
    port: u16,
    exclude_tunnel_id: Option<String>,
    state: tauri::State<'_, Arc<TunnelManagerState>>,
) -> Result<Value, String> {
    if port == 0 {
        return Err(StoreError::Validation("端口必须在 1-65535".into()).to_string());
    }
    let bind_label = format!("{host}:{port}");
    // 先查自家运行中隧道（它们持监听 socket，探测必撞）
    if let Some(holder) = state.mgr.list().into_iter().find(|t| t.bind == bind_label) {
        if exclude_tunnel_id.as_deref() == Some(holder.id.as_str()) {
            return Ok(json!({ "available": true, "selfOccupied": true }));
        }
        return Ok(json!({
            "available": false,
            "holder": holder.id,
            "suggestedPort": suggest_free_port(&host, port),
        }));
    }
    // 再实际探测（不带 SO_REUSEADDR，真实占用语义）
    match std::net::TcpListener::bind((host.as_str(), port)) {
        Ok(l) => {
            drop(l);
            Ok(json!({ "available": true }))
        }
        Err(_) => Ok(json!({
            "available": false,
            "suggestedPort": suggest_free_port(&host, port),
        })),
    }
}

/// 从 port+1 起向上探测至多 100 个端口，返回第一个可绑定的；找不到返回 None
fn suggest_free_port(host: &str, port: u16) -> Option<u16> {
    (1..=100u16).find_map(|off| {
        let p = port.checked_add(off)?;
        std::net::TcpListener::bind((host, p)).ok().map(|l| {
            drop(l);
            p
        })
    })
}
