//! MCP 服务端：让 AI 客户端（Claude Code 等）在已保存的 SSH 会话上执行命令。
//!
//! 传输为 MCP Streamable HTTP（2025-03-26 子集）：单端点 POST /mcp，
//! 每次请求独立 JSON 响应（不开 SSE 流）；仅绑定 127.0.0.1。
//! 工具面：list_sessions（列会话档案，不含凭据）、ssh_exec（一次性 Bulk
//! 连接跑命令，收集 stdout/stderr/exit code）。鉴权为 Bearer token
//!（设置键 mcp.token，空 = 不鉴权；首次启用自动生成随机 token）。
//! 生命周期由 lib.rs 装配：setup 时按 mcp.enabled/mcp.port/mcp.token 启动，
//! mcp_restart 命令供设置变更后重载。

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::sync::watch;

use core_ssh::{
    ConnClass, ConnectOptions, HostKeyCheck, HostKeyDecision, HostKeyPrompt, KeepaliveConfig,
    KnownHostsPolicy, SshConnection,
};
use core_store::Store;

/// 默认监听端口（设置键 mcp.port 未配时）
const DEFAULT_PORT: u16 = 17345;
/// ssh_exec 默认超时
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// ssh_exec 超时上限
const MAX_TIMEOUT_MS: u64 = 120_000;
/// stdout/stderr 各自的截断上限
const OUTPUT_CAP: usize = 64 * 1024;
/// 停止时等待优雅退出的上限，超时后 abort
const STOP_GRACE: Duration = Duration::from_secs(2);

const SESSION_ID_HEADER: HeaderName = HeaderName::from_static("mcp-session-id");

/// MCP 服务运行参数（读自 core-store settings KV）
struct McpConfig {
    enabled: bool,
    port: u16,
    token: Option<String>,
}

/// 设置值容忍 JSON bool/string/number 与裸文本（settings_set 一律 JSON 编码，
/// 但容忍外部直写库的情况）。
fn parse_bool_setting(raw: Option<&str>) -> bool {
    match raw {
        None => false,
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Bool(b)) => b,
            Ok(Value::String(s)) => s == "true",
            _ => raw.trim() == "true",
        },
    }
}

fn parse_port_setting(raw: Option<&str>) -> u16 {
    let n = match raw {
        None => return DEFAULT_PORT,
        Some(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Number(n)) => n.as_u64(),
            Ok(Value::String(s)) => s.trim().parse::<u64>().ok(),
            _ => raw.trim().parse::<u64>().ok(),
        },
    };
    match n {
        Some(v) if (1..=65535).contains(&v) => v as u16,
        _ => DEFAULT_PORT,
    }
}

fn parse_token_setting(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let token = match serde_json::from_str::<Value>(raw) {
        Ok(Value::String(s)) => s,
        _ => raw.trim().to_string(),
    };
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// 读 mcp.enabled / mcp.port / mcp.token 三个设置键
async fn read_mcp_config(store: &Store) -> Result<McpConfig, String> {
    let settings = store.settings();
    let enabled = settings
        .get("mcp.enabled")
        .await
        .map_err(|e| e.to_string())?;
    let port = settings.get("mcp.port").await.map_err(|e| e.to_string())?;
    let token = settings.get("mcp.token").await.map_err(|e| e.to_string())?;
    Ok(McpConfig {
        enabled: parse_bool_setting(enabled.as_deref()),
        port: parse_port_setting(port.as_deref()),
        token: parse_token_setting(token.as_deref()),
    })
}

/// 生成 32 字节随机 hex token（rand 为 workspace 既有随机源）
fn gen_token() -> String {
    let bytes: [u8; 32] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// token 缺省时生成随机 token 并尝试入库（入库失败仅告警，本次仍生效）
async fn ensure_token(store: &Store, token: Option<String>) -> String {
    if let Some(t) = token.filter(|t| !t.is_empty()) {
        return t;
    }
    let token = gen_token();
    let stored = serde_json::to_string(&Value::String(token.clone())).map_err(|e| e.to_string());
    match stored {
        Ok(text) => {
            if let Err(e) = store.settings().set("mcp.token", &text).await {
                tracing::warn!(error = %e, "MCP token 入库失败（本次运行仍使用生成的 token）");
            } else {
                tracing::info!("MCP 首次启用：已生成随机 token 存入 mcp.token");
            }
        }
        Err(e) => tracing::warn!(error = %e, "MCP token 序列化失败"),
    }
    token
}

/// MCP 服务生命周期管理：持有 tokio task + shutdown watch
#[derive(Default)]
pub struct McpManager {
    inner: Mutex<McpInner>,
}

#[derive(Default)]
struct McpInner {
    shutdown: Option<watch::Sender<bool>>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    port: u16,
    token: String,
}

impl McpManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 启动服务（已有实例先停）。绑定失败返回清晰错误，不 panic。
    pub async fn start(
        self: &Arc<Self>,
        store: Arc<Store>,
        port: u16,
        token: String,
    ) -> Result<(), String> {
        self.stop().await;
        let state = Arc::new(ServerState {
            store,
            token: token.clone(),
        });
        let router = Router::new()
            .route("/mcp", post(mcp_post))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .map_err(|e| format!("MCP 端口 127.0.0.1:{port} 绑定失败: {e}"))?;
        let (tx, mut rx) = watch::channel(false);
        let task = tauri::async_runtime::spawn(async move {
            let result = axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    loop {
                        match rx.changed().await {
                            // 发送端被丢弃同样视为停止信号
                            Ok(()) if *rx.borrow() => break,
                            // 初始值 false：等下一次变化；发送端被丢弃同样视为停止
                            Ok(()) => {}
                            Err(_) => break,
                        }
                    }
                })
                .await;
            if let Err(e) = result {
                tracing::error!(error = %e, "MCP HTTP 服务异常退出");
            }
        });
        let mut inner = self.inner.lock();
        inner.shutdown = Some(tx);
        inner.task = Some(task);
        inner.port = port;
        inner.token = token;
        tracing::info!(port, "MCP 服务已启动（127.0.0.1）");
        Ok(())
    }

    /// 停止服务：先发 shutdown 信号走优雅退出，超过 STOP_GRACE 则 abort
    pub async fn stop(&self) {
        let (tx, task) = {
            let mut inner = self.inner.lock();
            inner.port = 0;
            (inner.shutdown.take(), inner.task.take())
        };
        if let Some(tx) = tx {
            let _ = tx.send(true);
        }
        if let Some(mut task) = task {
            match tokio::time::timeout(STOP_GRACE, &mut task).await {
                Ok(_) => tracing::info!("MCP 服务已停止"),
                Err(_) => {
                    // 有挂起连接拖住优雅退出时直接 abort（无状态服务，安全）
                    task.abort();
                    tracing::warn!("MCP 服务优雅退出超时，强制结束");
                }
            }
        }
    }

    /// 当前状态：{running, port, token_set}
    pub fn status(&self) -> Value {
        let inner = self.inner.lock();
        // 句柄存在即视为运行中（stop 会取走句柄；服务崩溃属异常，下次 status 仍报 running）
        let running = inner.task.is_some();
        json!({
            "running": running,
            "port": if running { inner.port } else { 0 },
            "tokenSet": !inner.token.is_empty(),
        })
    }
}

/// setup 钩子：按当前设置启动（enabled 才动；绑定失败仅日志，不影响应用）
pub async fn boot_from_settings(mgr: Arc<McpManager>, store: Arc<Store>) {
    let cfg = match read_mcp_config(&store).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "读取 MCP 设置失败，跳过启动");
            return;
        }
    };
    if !cfg.enabled {
        return;
    }
    let token = ensure_token(&store, cfg.token).await;
    if let Err(e) = mgr.start(store, cfg.port, token).await {
        tracing::error!(error = %e, "MCP 服务启动失败");
    }
}

/// mcp_restart 命令实现：停旧实例，按当前 settings 重启（enabled=false 则仅停）
pub async fn restart_from_settings(
    mgr: &Arc<McpManager>,
    store: Arc<Store>,
) -> Result<Value, String> {
    mgr.stop().await;
    let cfg = read_mcp_config(&store).await?;
    if cfg.enabled {
        let token = ensure_token(&store, cfg.token).await;
        // 绑定失败直接回报前端，同时把 token 记入状态便于排查
        mgr.inner.lock().token = token.clone();
        mgr.start(store, cfg.port, token).await?;
    }
    Ok(mgr.status())
}

#[tauri::command]
pub async fn mcp_status(state: tauri::State<'_, Arc<McpManager>>) -> Result<Value, String> {
    Ok(state.status())
}

#[tauri::command]
pub async fn mcp_restart(
    state: tauri::State<'_, Arc<McpManager>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<Value, String> {
    restart_from_settings(&state, sessions.store.clone()).await
}

/// HTTP 服务共享状态
struct ServerState {
    store: Arc<Store>,
    token: String,
}

/// POST /mcp 入口：鉴权 → JSON-RPC 解析 → 分发
async fn mcp_post(
    State(st): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !st.token.is_empty() {
        let ok = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|t| t == st.token);
        if !ok {
            tracing::warn!("MCP 请求鉴权失败");
            return respond(
                StatusCode::UNAUTHORIZED,
                Some(rpc_error(
                    Value::Null,
                    -32001,
                    "未授权：需 Authorization: Bearer <token>",
                )),
                &headers,
            );
        }
    }

    let req: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            return respond(
                StatusCode::OK,
                Some(rpc_error(Value::Null, -32700, "JSON 解析失败")),
                &headers,
            );
        }
    };

    let id = req.get("id").cloned();
    let is_notification = id.is_none();
    let id = id.unwrap_or(Value::Null);
    let method = req.get("method").and_then(Value::as_str);
    let Some(method) = method.filter(|_| req.is_object()) else {
        return respond(
            StatusCode::OK,
            Some(rpc_error(id, -32600, "非法 JSON-RPC 请求")),
            &headers,
        );
    };

    let outcome = dispatch(&st, method, &req).await;
    match outcome {
        Outcome::Accepted => respond(StatusCode::ACCEPTED, None, &headers),
        Outcome::Result(result) => {
            if is_notification {
                respond(StatusCode::ACCEPTED, None, &headers)
            } else {
                respond(
                    StatusCode::OK,
                    Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
                    &headers,
                )
            }
        }
        Outcome::Error(code, msg) => {
            if is_notification {
                respond(StatusCode::ACCEPTED, None, &headers)
            } else {
                respond(StatusCode::OK, Some(rpc_error(id, code, &msg)), &headers)
            }
        }
    }
}

/// 构造响应：JSON 体 + 回显 Mcp-Session-Id（若有）
fn respond(status: StatusCode, value: Option<Value>, req_headers: &HeaderMap) -> Response {
    let mut resp = match value {
        Some(v) => (status, Json(v)).into_response(),
        None => status.into_response(),
    };
    if let Some(hv) = req_headers
        .get(&SESSION_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| HeaderValue::from_str(s).ok())
    {
        resp.headers_mut().insert(&SESSION_ID_HEADER, hv);
    }
    resp
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

enum Outcome {
    /// 通知：202 空响应
    Accepted,
    /// JSON-RPC result 载荷
    Result(Value),
    /// JSON-RPC error（code, message）
    Error(i64, String),
}

/// JSON-RPC 方法分发（Streamable HTTP 2025-03-26 子集）
async fn dispatch(st: &ServerState, method: &str, req: &Value) -> Outcome {
    match method {
        "initialize" => Outcome::Result(json!({
            "protocolVersion": "2025-03-26",
            "serverInfo": { "name": "myssh", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "tools": {} },
        })),
        // 客户端就绪通知：直接 202
        "notifications/initialized" => Outcome::Accepted,
        "ping" => Outcome::Result(json!({})),
        "tools/list" => Outcome::Result(tools_list()),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(Value::as_str);
            let Some(name) = name else {
                return Outcome::Error(-32602, "tools/call 缺少 params.name".into());
            };
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            Outcome::Result(call_tool(st, name, &args).await)
        }
        // 其他通知直接吞掉；有 id 的未知方法报 -32601
        m if m.starts_with("notifications/") => Outcome::Accepted,
        _ => Outcome::Error(-32601, format!("未知方法：{method}")),
    }
}

/// v1 工具面：两个工具的 JSON Schema
fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "list_sessions",
                "description": "列出 mySSH 已保存的会话档案（id/name/host/port/user/group/kind），不含凭据",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false
                }
            },
            {
                "name": "ssh_exec",
                "description": "在已保存的 SSH 会话上执行一条 shell 命令，返回 stdout/stderr/exit code。仅 SSH 会话；本地会话会被拒绝。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": {
                            "type": "string",
                            "description": "会话档案 id（list_sessions 返回的 id）"
                        },
                        "command": {
                            "type": "string",
                            "description": "要在远端执行的 shell 命令"
                        },
                        "timeout_ms": {
                            "type": "integer",
                            "description": "超时毫秒数，默认 30000，上限 120000",
                            "minimum": 1,
                            "maximum": 120000,
                            "default": 30000
                        }
                    },
                    "required": ["session_id", "command"],
                    "additionalProperties": false
                }
            }
        ]
    })
}

/// MCP content 结果包装
fn ok_content(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn err_content(msg: impl Into<String>) -> Value {
    let mut v = ok_content(msg.into());
    v["isError"] = Value::Bool(true);
    v
}

/// tools/call 分发：未知工具报 JSON-RPC 层错误之外，工具执行失败走 isError 内容
async fn call_tool(st: &ServerState, name: &str, args: &Value) -> Value {
    match name {
        "list_sessions" => match list_sessions_tool(&st.store).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        "ssh_exec" => match ssh_exec_tool(&st.store, args).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        other => err_content(format!("未知工具：{other}")),
    }
}

/// list_sessions：会话档案最小投影，绝不带凭据材料
async fn list_sessions_tool(store: &Arc<Store>) -> Result<String, String> {
    let list = store.sessions().list().await.map_err(|e| e.to_string())?;
    let items: Vec<Value> = list
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "name": r.name,
                "host": r.host,
                "port": r.port,
                "user": r.user,
                "group": r.group_path,
                "kind": r.kind.as_str(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({ "sessions": items })).map_err(|e| e.to_string())
}

/// ssh_exec：resolve → 一次性 Bulk 连接 → exec channel 收集 stdout/stderr/exit
async fn ssh_exec_tool(store: &Arc<Store>, args: &Value) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "ssh_exec 缺少参数 session_id".to_string())?;
    let command = args
        .get("command")
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .ok_or_else(|| "ssh_exec 缺少参数 command".to_string())?;
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(1, MAX_TIMEOUT_MS);

    let target = crate::sessions::resolve_session_target(store, session_id).await?;
    let spec = match target {
        crate::sessions::ResolvedTarget::Ssh(s) => s,
        crate::sessions::ResolvedTarget::Local(_) => {
            return Err("本地会话不支持 ssh_exec（仅 SSH 会话）".into());
        }
    };

    tracing::info!(
        session_id,
        host = %spec.host,
        timeout_ms,
        "MCP ssh_exec 开始执行"
    );
    let started = Instant::now();
    let run = ssh_exec_inner(spec, command.to_string());
    let result = tokio::time::timeout(Duration::from_millis(timeout_ms), run).await;
    let elapsed = started.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(mut body)) => {
            body["durationMs"] = json!(elapsed);
            serde_json::to_string_pretty(&body).map_err(|e| e.to_string())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!("命令执行超时（{timeout_ms} ms），连接已断开")),
    }
}

/// 建连 + exec + 输出收集（被外层 timeout 包裹；超时 drop 即断连）
async fn ssh_exec_inner(
    spec: crate::terminal::TermOpenSpec,
    command: String,
) -> Result<Value, String> {
    let auth = crate::terminal::auth_method_from(&spec.auth);
    let jump_chain = crate::terminal::jump_chain_from(&spec.jump_chain);
    let opts = ConnectOptions {
        host: spec.host.clone(),
        port: spec.port,
        user: spec.user.clone(),
        auth,
        jump_chain,
        // Bulk 语义：不占交互连接，与监控/测试连接一致
        class: ConnClass::Bulk,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        // 无 UI 弹窗通路：已知主机直过；未知/变更 fail-closed 拒绝，
        // 提示用户先在 mySSH UI 首连确认指纹
        host_key_check: HostKeyCheck::KnownHosts(KnownHostsPolicy {
            path: crate::terminal::known_hosts_path(),
            prompter: Arc::new(|_: HostKeyPrompt| async { HostKeyDecision::Reject }),
        }),
        // KI 无应答通路：ki_prompter 缺省时认证失败会原样回报
        ki_prompter: None,
    };
    let conn = SshConnection::connect(opts).await.map_err(|e| {
        let msg = e.to_string();
        if msg.contains("主机密钥") {
            format!("{msg}（未知/变更的主机密钥：请先在 mySSH UI 中连接一次该会话以确认指纹）")
        } else {
            msg
        }
    })?;

    let mut ch = conn
        .open_session_channel()
        .await
        .map_err(|e| e.to_string())?;
    ch.exec(true, command.as_str())
        .await
        .map_err(|e| format!("exec 通道请求失败: {e}"))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    let mut exit_status: Option<u32> = None;
    while let Some(msg) = ch.wait().await {
        match msg {
            russh::ChannelMsg::Data { data } => push_capped(&mut stdout, &data, &mut truncated),
            russh::ChannelMsg::ExtendedData { data, .. } => {
                push_capped(&mut stderr, &data, &mut truncated);
            }
            russh::ChannelMsg::ExitStatus { exit_status: code } => exit_status = Some(code),
            russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
            _ => {}
        }
    }
    drop(conn);

    Ok(json!({
        "exitCode": exit_status,
        "stdout": String::from_utf8_lossy(&stdout),
        "stderr": String::from_utf8_lossy(&stderr),
        "truncated": truncated,
    }))
}

/// 追加到缓冲，超过 OUTPUT_CAP 的部分丢弃并置截断标记
fn push_capped(buf: &mut Vec<u8>, data: &[u8], truncated: &mut bool) {
    let room = OUTPUT_CAP.saturating_sub(buf.len());
    if room >= data.len() {
        buf.extend_from_slice(data);
    } else {
        buf.extend_from_slice(&data[..room]);
        *truncated = true;
    }
}
