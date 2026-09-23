//! MCP 服务端：让 AI 客户端（Claude Code 等）在已保存的 SSH 会话上执行命令。
//!
//! 传输为 MCP Streamable HTTP（2025-03-26 子集）：单端点 POST /mcp，
//! 每次请求独立 JSON 响应（不开 SSE 流）；仅绑定 127.0.0.1。
//! 工具面：list_sessions（列会话档案，不含凭据）、ssh_exec（一次性 Bulk
//! 连接跑命令，收集 stdout/stderr/exit code）、terminal_*（open/send/read/
//! close 交互式有状态 shell，实现见 mcp_terminal.rs）、sftp_*（home/list/stat/read/
//! write/mkdir/delete/rename/chmod，与 UI 共享 SFTP Bulk 连接池，写操作落
//! audit 且 actor=mcp；upload/download 入队 UI 同款 TransferQueue 后台执行，
//! 进度用 transfer_list 轮询）。鉴权为 Bearer token
//!（设置键 mcp.token，空 = 不鉴权；首次启用自动生成随机 token）。
//! 工具分组权限由设置键 mcp.allow.{list_sessions,ssh_exec,sftp_read,
//! sftp_write,sftp_transfer} 控制（terminal_* 归入 ssh_exec 组；缺省全开；
//! tools/list 同步过滤，tools/call 命中禁用组返回 isError）。会话编辑器可逐组覆盖
//!（sessions.mcp_perms 稀疏映射），会话级覆盖优先于全局。
//! 生命周期由 lib.rs 装配：setup 时按 mcp.enabled/mcp.port/mcp.token 启动，
//! mcp_restart 命令供设置变更后重载。

use std::path::PathBuf;
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

use core_sftp::OnExists;
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
    perms: McpPerms,
}

/// 工具分组权限（设置键 mcp.allow.*，缺省 = 允许：旧库无这些键时行为不变）。
/// 粒度为分组而非单工具：18 个工具逐一切换噪音过大，读写分离已覆盖风险面。
#[derive(Clone, Copy)]
pub struct McpPerms {
    list_sessions: bool,
    ssh_exec: bool,
    /// sftp_home/list/stat/read
    sftp_read: bool,
    /// sftp_write/mkdir/delete/rename/chmod
    sftp_write: bool,
    /// sftp_upload/download（触及本地文件系统，单独成组）
    sftp_transfer: bool,
}

impl Default for McpPerms {
    fn default() -> Self {
        Self {
            list_sessions: true,
            ssh_exec: true,
            sftp_read: true,
            sftp_write: true,
            sftp_transfer: true,
        }
    }
}

/// 工具名 → 权限分组键（全局 mcp.allow.* 与会话 mcp_perms 覆盖共用同一组键名）
fn tool_group_key(name: &str) -> Option<&'static str> {
    match name {
        "list_sessions" => Some("list_sessions"),
        "ssh_exec" => Some("ssh_exec"),
        "terminal_open" => Some("ssh_exec"),
        "terminal_send" => Some("ssh_exec"),
        "terminal_read" => Some("ssh_exec"),
        "terminal_close" => Some("ssh_exec"),
        "sftp_home" | "sftp_list" | "sftp_stat" | "sftp_read" => Some("sftp_read"),
        "sftp_write" | "sftp_mkdir" | "sftp_delete" | "sftp_rename" | "sftp_chmod" => {
            Some("sftp_write")
        }
        "sftp_upload" | "sftp_download" | "sftp_transfer_list" => Some("sftp_transfer"),
        _ => None,
    }
}

impl McpPerms {
    /// 工具名 → 全局是否放行；未知名不拦截（交给原有「未知工具」分支）
    fn allows(&self, name: &str) -> bool {
        match tool_group_key(name) {
            Some("list_sessions") => self.list_sessions,
            Some("ssh_exec") => self.ssh_exec,
            Some("sftp_read") => self.sftp_read,
            Some("sftp_write") => self.sftp_write,
            Some("sftp_transfer") => self.sftp_transfer,
            _ => true,
        }
    }
}

/// 有效权限：会话级覆盖（sessions.mcp_perms）优先，缺省回退全局快照。
/// 会话不存在时按全局放行——工具自身会报 NotFound。
async fn tool_allowed(st: &ServerState, name: &str, args: &Value) -> bool {
    let global = st.perms.allows(name);
    let Some(key) = tool_group_key(name) else {
        return global;
    };
    let Some(sid) = args.get("session_id").and_then(Value::as_str) else {
        return global;
    };
    match st.store.sessions().get(sid).await {
        Ok(rec) => rec.mcp_perms.get(key).copied().unwrap_or(global),
        Err(_) => global,
    }
}

/// mcp.allow.* 缺省视为允许（向后兼容），其余走 bool 解析
fn parse_allow_setting(raw: Option<&str>) -> bool {
    raw.is_none_or(|r| parse_bool_setting(Some(r)))
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

/// 读 mcp.enabled / mcp.port / mcp.token 与 mcp.allow.* 权限键
async fn read_mcp_config(store: &Store) -> Result<McpConfig, String> {
    let settings = store.settings();
    let get = async |key: &str| settings.get(key).await.map_err(|e| e.to_string());
    let enabled = get("mcp.enabled").await?;
    let port = get("mcp.port").await?;
    let token = get("mcp.token").await?;
    let perms = McpPerms {
        list_sessions: parse_allow_setting(get("mcp.allow.list_sessions").await?.as_deref()),
        ssh_exec: parse_allow_setting(get("mcp.allow.ssh_exec").await?.as_deref()),
        sftp_read: parse_allow_setting(get("mcp.allow.sftp_read").await?.as_deref()),
        sftp_write: parse_allow_setting(get("mcp.allow.sftp_write").await?.as_deref()),
        sftp_transfer: parse_allow_setting(get("mcp.allow.sftp_transfer").await?.as_deref()),
    };
    Ok(McpConfig {
        enabled: parse_bool_setting(enabled.as_deref()),
        port: parse_port_setting(port.as_deref()),
        token: parse_token_setting(token.as_deref()),
        perms,
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
    /// 交互式终端注册表（start 时挂入，stop 清场）
    terms: Option<Arc<crate::mcp_terminal::McpTerminalState>>,
}

impl McpManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 启动服务（已有实例先停）。绑定失败返回清晰错误，不 panic。
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        self: &Arc<Self>,
        store: Arc<Store>,
        sftp: Arc<crate::sftp::SftpManagerState>,
        exec: Arc<crate::exec::ExecManagerState>,
        terms: Arc<crate::mcp_terminal::McpTerminalState>,
        port: u16,
        token: String,
        perms: McpPerms,
    ) -> Result<(), String> {
        self.stop().await;
        let state = Arc::new(ServerState {
            store,
            sftp,
            exec,
            terms: terms.clone(),
            token: token.clone(),
            perms,
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
        inner.terms = Some(terms);
        tracing::info!(port, "MCP 服务已启动（127.0.0.1）");
        Ok(())
    }

    /// 停止服务：先发 shutdown 信号走优雅退出，超过 STOP_GRACE 则 abort
    pub async fn stop(&self) {
        let (tx, task, terms) = {
            let mut inner = self.inner.lock();
            inner.port = 0;
            (inner.shutdown.take(), inner.task.take(), inner.terms.take())
        };
        // 交互式终端清场：关通道 + abort 读任务，不留孤儿 Bulk 连接
        if let Some(terms) = terms {
            terms.close_all().await;
        }
        if let Some(tx) = tx {
            let _ = tx.send(true);
        }
        if let Some(mut task) = task {
            match tokio::time::timeout(STOP_GRACE, &mut task).await {
                Ok(_) => tracing::info!("MCP 服务已停止"),
                Err(_) => {
                    // 有挂起连接拖住优雅退出时直接 abort（终端已清场，其余请求无状态）
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

pub async fn boot_from_settings(
    mgr: Arc<McpManager>,
    store: Arc<Store>,
    sftp: Arc<crate::sftp::SftpManagerState>,
    exec: Arc<crate::exec::ExecManagerState>,
    terms: Arc<crate::mcp_terminal::McpTerminalState>,
) {
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
    if let Err(e) = mgr
        .start(store, sftp, exec, terms, cfg.port, token, cfg.perms)
        .await
    {
        tracing::error!(error = %e, "MCP 服务启动失败");
    }
}

pub async fn restart_from_settings(
    mgr: &Arc<McpManager>,
    store: Arc<Store>,
    sftp: Arc<crate::sftp::SftpManagerState>,
    exec: Arc<crate::exec::ExecManagerState>,
    terms: Arc<crate::mcp_terminal::McpTerminalState>,
) -> Result<Value, String> {
    mgr.stop().await;
    let cfg = read_mcp_config(&store).await?;
    if cfg.enabled {
        let token = ensure_token(&store, cfg.token).await;
        // 绑定失败直接回报前端，同时把 token 记入状态便于排查
        mgr.inner.lock().token = token.clone();
        mgr.start(store, sftp, exec, terms, cfg.port, token, cfg.perms)
            .await?;
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
    sftp: tauri::State<'_, Arc<crate::sftp::SftpManagerState>>,
    exec: tauri::State<'_, Arc<crate::exec::ExecManagerState>>,
    terms: tauri::State<'_, Arc<crate::mcp_terminal::McpTerminalState>>,
) -> Result<Value, String> {
    restart_from_settings(
        &state,
        sessions.store.clone(),
        sftp.inner().clone(),
        exec.inner().clone(),
        terms.inner().clone(),
    )
    .await
}

/// HTTP 服务共享状态
struct ServerState {
    store: Arc<Store>,
    /// SFTP 连接池（与 UI 共享；agent 高频小操作复用 Bulk 连接，不反复握手）
    sftp: Arc<crate::sftp::SftpManagerState>,
    /// Exec Transport 组（ssh_exec 复用；PR-14 起与 Monitor 共享，MCP 配额 4）
    exec: Arc<crate::exec::ExecManagerState>,
    /// 交互式终端注册表（terminal_* 工具；stop 时 close_all 清场）
    terms: Arc<crate::mcp_terminal::McpTerminalState>,
    token: String,
    /// 工具分组权限（启动时快照；改设置后 mcp_restart 生效）
    perms: McpPerms,
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
        "tools/list" => Outcome::Result(tools_list(&st.perms)),
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

/// 工具面 JSON Schema；按分组权限过滤后返回（禁用的工具对客户端不可见）
fn tools_list(perms: &McpPerms) -> Value {
    let mut v = json!({
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
            },
            {
                "name": "terminal_open",
                "description": "在已保存的 SSH 会话上打开交互式终端（有状态 shell：cd、进 mysql、激活 venv 等状态跨调用保持），返回 terminalId。独立 Bulk 连接，xterm 120x32。仅 SSH 会话；本地会话与 keyboard-interactive 认证会被拒绝。输出用 terminal_read 轮询，用完用 terminal_close 释放。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string", "description": "会话档案 id（list_sessions 返回的 id）" }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "terminal_send",
                "description": "向 terminal_open 打开的终端写入 UTF-8 文本（命令、交互应答等）。换行需自行包含在 data 中（\\n），或置 append_newline=true 末尾自动补一个（模拟回车）。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "terminalId": { "type": "string", "description": "terminal_open 返回的终端 id" },
                        "data": { "type": "string", "description": "要写入的 UTF-8 文本" },
                        "append_newline": { "type": "boolean", "default": false, "description": "末尾追加 \\n" }
                    },
                    "required": ["terminalId", "data"],
                    "additionalProperties": false
                }
            },
            {
                "name": "terminal_read",
                "description": "读取终端输出。offset 游标模型：返回 offset 之后的输出与 nextOffset（下次传入继续读；首次传 0）。输出缓冲为 1MiB 环形，超界丢最旧；offset 早于可用起点时 truncated=true 并从最早可用处返回。wait_seconds>0 时服务端长轮询（最长 600s）直到有新输出或终端关闭。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "terminalId": { "type": "string" },
                        "offset": { "type": "integer", "minimum": 0, "description": "上次返回的 nextOffset；首次传 0" },
                        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 600, "default": 0 }
                    },
                    "required": ["terminalId", "offset"],
                    "additionalProperties": false
                }
            },
            {
                "name": "terminal_close",
                "description": "关闭终端：断通道、终止读任务、释放连接（落审计）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "terminalId": { "type": "string" }
                    },
                    "required": ["terminalId"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_home",
                "description": "解析会话远端家目录绝对路径",
                "inputSchema": {
                    "type": "object",
                    "properties": { "session_id": { "type": "string" } },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_list",
                "description": "列出远端目录内容（name/path/kind/size/mtime/permissions）。SFTP 操作与 UI 共享 Bulk 连接池。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string", "description": "远端目录绝对路径" }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_stat",
                "description": "远端路径元数据（不跟随软链接）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_read",
                "description": "读远端文本文件（默认上限 256KiB，max_bytes 最大 1MiB；超出置 truncated=true）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 262144 }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_write",
                "description": "覆盖写远端文件（不存在则创建；内容上限 1MiB）。注意：会整体替换文件内容。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "content": { "type": "string", "description": "完整新内容（UTF-8）" }
                    },
                    "required": ["session_id", "path", "content"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_mkdir",
                "description": "远端新建目录",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_delete",
                "description": "删除远端文件/目录；目录需 recursive=true 才递归删除",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "recursive": { "type": "boolean", "default": false }
                    },
                    "required": ["session_id", "path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_rename",
                "description": "远端移动/重命名",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "from": { "type": "string" },
                        "to": { "type": "string" }
                    },
                    "required": ["session_id", "from", "to"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_chmod",
                "description": "改远端权限，mode 为八进制字符串（如 \"755\"）",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "path": { "type": "string" },
                        "mode": { "type": "string", "description": "八进制权限，如 755 / 644" }
                    },
                    "required": ["session_id", "path", "mode"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_upload",
                "description": "上传本地文件到远端（覆盖写；入队后台传输队列执行，与 UI 传输面板共享，无大小上限）。wait_seconds>0 时同步等待终态返回（最长 600s，等待不占调用方上下文）；否则立即返回 queued 态，进度用 sftp_transfer_list 查询。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "local_path": { "type": "string", "description": "本地文件绝对路径" },
                        "remote_path": { "type": "string", "description": "远端目标绝对路径" },
                        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 600, "default": 0, "description": "同步等待传输终态的秒数；0 = 立即返回" }
                    },
                    "required": ["session_id", "local_path", "remote_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_download",
                "description": "下载远端文件到本地（覆盖写本地目标；入队后台传输队列执行，无大小上限）。wait_seconds 语义同 sftp_upload。",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" },
                        "remote_path": { "type": "string", "description": "远端文件绝对路径" },
                        "local_path": { "type": "string", "description": "本地目标绝对路径" },
                        "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 600, "default": 0 }
                    },
                    "required": ["session_id", "remote_path", "local_path"],
                    "additionalProperties": false
                }
            },
            {
                "name": "sftp_transfer_list",
                "description": "列出会话当前传输任务（id/方向/状态 queued|running|paused|done|failed|canceled/进度/错误），用于轮询 sftp_upload/download 的进度",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "session_id": { "type": "string" }
                    },
                    "required": ["session_id"],
                    "additionalProperties": false
                }
            }
        ]
    });
    if let Some(tools) = v.get_mut("tools").and_then(Value::as_array_mut) {
        tools.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| perms.allows(n))
        });
    }
    v
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

/// tools/call 分发：先过权限闸（设置 → MCP 工具权限），未知工具报 JSON-RPC 层错误之外，
/// 工具执行失败走 isError 内容
async fn call_tool(st: &ServerState, name: &str, args: &Value) -> Value {
    if !tool_allowed(st, name, args).await {
        return err_content(format!(
            "MCP 工具已被禁用：{name}（mySSH 设置 → MCP 工具权限，或会话编辑器的 MCP 权限覆盖）"
        ));
    }
    match name {
        "list_sessions" => match list_sessions_tool(&st.store).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        "ssh_exec" => match ssh_exec_tool(st, args).await {
            Ok(text) => ok_content(text),
            Err(e) => err_content(e),
        },
        // 交互式终端工具族（有状态 shell；实现见 mcp_terminal.rs）
        n if n.starts_with("terminal_") => {
            match crate::mcp_terminal::terminal_tool(&st.store, &st.terms, n, args).await {
                Ok(text) => ok_content(text),
                Err(e) => err_content(e),
            }
        }
        // SFTP 工具族：30s 超时兜底，错误走 isError 内容
        n if n.starts_with("sftp_") => match sftp_tool(st, n, args).await {
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

/// ssh_exec：resolve → Exec Transport（PR-14 复用连接，MCP 配额）→ exec channel 收集
async fn ssh_exec_tool(st: &ServerState, args: &Value) -> Result<String, String> {
    let store = &st.store;
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
    if matches!(target, crate::sessions::ResolvedTarget::Local(_)) {
        return Err("本地会话不支持 ssh_exec（仅 SSH 会话）".into());
    }

    tracing::info!(session_id, timeout_ms, "MCP ssh_exec 开始执行");
    let started = Instant::now();
    let run = async {
        let ctx = crate::exec::ensure_exec_ctx(&st.exec, store, session_id).await?;
        let _permit = ctx.acquire_mcp().await?;
        ssh_exec_inner(ctx, command.to_string()).await
    };
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
async fn ssh_exec_inner(ctx: Arc<crate::exec::ExecCtx>, command: String) -> Result<Value, String> {
    // Exec Transport 复用连接（PR-14）：建连/host-key/KI 策略由 exec 组工厂统一承载；
    // 超时 drop 只弃本 channel，共享 Transport 存活
    let conn = ctx.conn_handle();
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

// ---------- SFTP 工具族 ----------
/// sftp_read 默认/最大返回字节；sftp_write 内容上限（字节要进模型上下文，必须有界）
const SFTP_READ_DEFAULT_CAP: u64 = 256 * 1024;
const SFTP_IO_MAX: u64 = 1024 * 1024;
/// 单操作超时（浏览/元操作/小文件读写；上传/下载入队即返回，wait_seconds 另加预算）
const SFTP_OP_TIMEOUT: Duration = Duration::from_secs(30);

/// 解析 session_id 并取/建 SFTP 上下文（ensure_ctx 内含 KI 拒绝与连接复用）
async fn sftp_ctx(
    st: &ServerState,
    args: &Value,
    tool: &str,
) -> Result<(String, Arc<crate::sftp::SftpCtx>), String> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 session_id"))?;
    let ctx = crate::sftp::ensure_ctx(&st.sftp, &st.store, session_id).await?;
    Ok((session_id.to_string(), ctx))
}

fn req_str<'a>(args: &'a Value, key: &str, tool: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 {key}"))
}

/// MCP 侧写操作审计（Actor::Mcp，与 UI 的 Actor::Gui 区分来源）
async fn mcp_audit(st: &ServerState, session_id: &str, action: &str, detail: &Value) {
    let _ = st
        .store
        .audit()
        .append(core_store::Actor::Mcp, Some(session_id), action, detail)
        .await;
}

/// SFTP 工具分发（外层 call_tool 已按 sftp_ 前缀过滤；此处再校验具体名）。
/// 均为快操作；仅上传/下载可带 wait_seconds 同步等待终态，超时预算随之放宽。
async fn sftp_tool(st: &ServerState, name: &str, args: &Value) -> Result<String, String> {
    let wait = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(600);
    let budget = SFTP_OP_TIMEOUT + Duration::from_secs(wait);
    let run = sftp_tool_inner(st, name, args);
    match tokio::time::timeout(budget, run).await {
        Ok(r) => r,
        Err(_) => Err(format!("SFTP 操作超时（{}s）", budget.as_secs())),
    }
}

async fn sftp_tool_inner(st: &ServerState, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "sftp_home" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let mc = ctx.meta_client().await?;
            let home = crate::sftp::resolve_home_abs(&mc).await?;
            Ok(json!({ "path": home }).to_string())
        }
        "sftp_list" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let p = path.to_string();
            let entries = crate::sftp::meta(&ctx, |c| async move { c.list(&p).await }).await?;
            let items: Vec<Value> = entries.iter().map(crate::sftp::entry_to_json).collect();
            Ok(json!({ "path": path, "entries": items }).to_string())
        }
        "sftp_stat" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let p = path.to_string();
            let e = crate::sftp::meta(&ctx, |c| async move { c.lstat(&p).await }).await?;
            Ok(crate::sftp::entry_to_json(&e).to_string())
        }
        "sftp_read" => {
            use tokio::io::AsyncReadExt;
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let max = args
                .get("max_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(SFTP_READ_DEFAULT_CAP)
                .clamp(1, SFTP_IO_MAX);
            let p = path.to_string();
            let f = crate::sftp::meta(&ctx, |c| async move { c.open_read(&p).await }).await?;
            let mut buf = Vec::new();
            f.take(max + 1)
                .read_to_end(&mut buf)
                .await
                .map_err(|e| e.to_string())?;
            let truncated = buf.len() as u64 > max;
            buf.truncate(max as usize);
            Ok(json!({
                "path": path,
                "bytes": buf.len(),
                "truncated": truncated,
                "content": String::from_utf8_lossy(&buf),
            })
            .to_string())
        }
        "sftp_write" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .ok_or_else(|| "sftp_write 缺少参数 content".to_string())?;
            if content.len() as u64 > SFTP_IO_MAX {
                return Err(format!(
                    "content 超过上限 {} 字节（大文件请用 UI 传输队列）",
                    SFTP_IO_MAX
                ));
            }
            let p = path.to_string();
            let bytes = content.as_bytes();
            crate::sftp::meta(&ctx, move |c| async move { c.overwrite(&p, bytes).await }).await?;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_write",
                &json!({ "path": path, "bytes": content.len() }),
            )
            .await;
            Ok(json!({ "path": path, "bytes": content.len() }).to_string())
        }
        "sftp_mkdir" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let p = path.to_string();
            crate::sftp::meta(&ctx, move |c| async move { c.mkdir(&p).await }).await?;
            mcp_audit(st, &sid, "mcp_sftp_mkdir", &json!({ "path": path })).await;
            Ok(json!({ "ok": true, "path": path }).to_string())
        }
        "sftp_delete" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let recursive = args
                .get("recursive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let p = path.to_string();
            let st_meta = crate::sftp::meta(&ctx, {
                let p = p.clone();
                move |c| async move { c.lstat(&p).await }
            })
            .await?;
            match st_meta.kind {
                core_sftp::EntryKind::Dir => {
                    if recursive {
                        crate::sftp::meta(
                            &ctx,
                            move |c| async move { c.remove_recursive(&p).await },
                        )
                        .await?;
                    } else {
                        crate::sftp::meta(&ctx, move |c| async move { c.remove_dir(&p).await })
                            .await?;
                    }
                }
                _ => {
                    crate::sftp::meta(&ctx, move |c| async move { c.remove_file(&p).await }).await?
                }
            }
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_delete",
                &json!({ "path": path, "recursive": recursive }),
            )
            .await;
            Ok(json!({ "ok": true, "path": path }).to_string())
        }
        "sftp_rename" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let from = req_str(args, "from", name)?;
            let to = req_str(args, "to", name)?;
            let f = from.to_string();
            let t = to.to_string();
            crate::sftp::meta(&ctx, move |c| async move { c.rename(&f, &t).await }).await?;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_rename",
                &json!({ "from": from, "to": to }),
            )
            .await;
            Ok(json!({ "ok": true, "from": from, "to": to }).to_string())
        }
        "sftp_chmod" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let path = req_str(args, "path", name)?;
            let mode_raw = req_str(args, "mode", name)?;
            let mode = u32::from_str_radix(mode_raw, 8)
                .map_err(|_| format!("mode 需为八进制字符串（如 755），收到: {mode_raw}"))?;
            let p = path.to_string();
            crate::sftp::meta(&ctx, move |c| async move { c.chmod(&p, mode).await }).await?;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_chmod",
                &json!({ "path": path, "mode": mode_raw }),
            )
            .await;
            Ok(json!({ "ok": true, "path": path, "mode": mode_raw }).to_string())
        }
        "sftp_upload" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let local = req_str(args, "local_path", name)?;
            let remote = req_str(args, "remote_path", name)?;
            let meta = tokio::fs::metadata(local)
                .await
                .map_err(|e| format!("本地文件读取失败 {local}: {e}"))?;
            if !meta.is_file() {
                return Err(format!("本地路径不是文件（目录上传请用 UI）：{local}"));
            }
            // 入队后台传输队列（与 UI 同一队列：进度进传输面板、终态落 transfers 表）；
            // 无大小上限——字节不走模型上下文，全程文件到文件流式
            let id = ctx
                .queue()
                .enqueue_upload(
                    PathBuf::from(local),
                    remote.to_string(),
                    meta.len(),
                    OnExists::Overwrite,
                )
                .await;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_upload",
                &json!({ "local": local, "remote": remote, "bytes": meta.len(), "transferId": id }),
            )
            .await;
            wait_transfer(&ctx, &id, args).await
        }
        "sftp_download" => {
            let (sid, ctx) = sftp_ctx(st, args, name).await?;
            let remote = req_str(args, "remote_path", name)?;
            let local = req_str(args, "local_path", name)?;
            // 预检远端存在性与大小（入队需要 bytes_total；缺失/超限在入队前报错）
            let r = remote.to_string();
            let meta = crate::sftp::meta(&ctx, move |c| async move { c.stat(&r).await }).await?;
            if meta.kind == core_sftp::EntryKind::Dir {
                return Err(format!("远端路径是目录（目录下载请用 UI）：{remote}"));
            }
            let id = ctx
                .queue()
                .enqueue_download(
                    remote.to_string(),
                    PathBuf::from(local),
                    meta.size,
                    OnExists::Overwrite,
                )
                .await;
            mcp_audit(
                st,
                &sid,
                "mcp_sftp_download",
                &json!({ "remote": remote, "local": local, "bytes": meta.size, "transferId": id }),
            )
            .await;
            wait_transfer(&ctx, &id, args).await
        }
        "sftp_transfer_list" => {
            let (_sid, ctx) = sftp_ctx(st, args, name).await?;
            let items: Vec<Value> = ctx
                .queue()
                .list()
                .iter()
                .map(crate::sftp::transfer_to_json)
                .collect();
            Ok(json!({ "transfers": items }).to_string())
        }
        other => Err(format!("未知工具：{other}")),
    }
}

/// 入队后按需同步等待终态（wait_seconds 缺省 0 = 立即返回排队态）。
/// 等待在服务端轮询完成，不往模型上下文塞进度；超时未完结返回当前态（可再用
/// sftp_transfer_list 轮询）。
async fn wait_transfer(
    ctx: &Arc<crate::sftp::SftpCtx>,
    id: &str,
    args: &Value,
) -> Result<String, String> {
    let wait = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(600);
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        let t = ctx
            .queue()
            .list()
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| format!("传输 {id} 不存在"))?;
        if t.state.is_terminal() || Instant::now() >= deadline {
            return Ok(crate::sftp::transfer_to_json(&t).to_string());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    // 测试代码豁免 unwrap/expect（工作区红线仅约束非测试代码）
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    #[test]
    fn perms_default_allows_everything() {
        let p = McpPerms::default();
        for name in [
            "list_sessions",
            "ssh_exec",
            "sftp_home",
            "sftp_list",
            "sftp_stat",
            "sftp_read",
            "sftp_write",
            "sftp_mkdir",
            "sftp_delete",
            "sftp_rename",
            "sftp_chmod",
            "sftp_upload",
            "sftp_download",
            "sftp_transfer_list",
            "terminal_open",
            "terminal_send",
            "terminal_read",
            "terminal_close",
        ] {
            assert!(p.allows(name), "默认应放行 {name}");
        }
    }

    #[test]
    fn perms_group_mapping() {
        let p = McpPerms {
            list_sessions: false,
            ssh_exec: false,
            sftp_read: false,
            sftp_write: false,
            sftp_transfer: false,
        };
        assert!(!p.allows("list_sessions"));
        assert!(!p.allows("ssh_exec"));
        assert!(!p.allows("sftp_read"));
        assert!(!p.allows("sftp_stat"));
        assert!(!p.allows("sftp_delete"));
        assert!(!p.allows("sftp_upload"));
        assert!(!p.allows("sftp_download"));
        assert!(!p.allows("sftp_transfer_list"));
        // terminal_* 归入 ssh_exec 组
        assert!(!p.allows("terminal_open"));
        assert!(!p.allows("terminal_send"));
        assert!(!p.allows("terminal_read"));
        assert!(!p.allows("terminal_close"));
        // 未知名不被权限闸拦截（走「未知工具」分支）
        assert!(p.allows("sftp_nope"));
    }

    #[test]
    fn allow_setting_absent_means_allow() {
        assert!(parse_allow_setting(None));
        assert!(parse_allow_setting(Some("true")));
        assert!(!parse_allow_setting(Some("false")));
        // settings_set 落库为 JSON 编码
        assert!(parse_allow_setting(Some("\"true\"")));
    }

    #[test]
    fn tools_list_filters_disabled_groups() {
        // 提取工具名列表；结构不符时给出空表，由后续 len 断言失败暴露
        fn names_of(v: &Value) -> Vec<&str> {
            v["tools"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter_map(|t| t["name"].as_str())
                .collect()
        }
        let all = tools_list(&McpPerms::default());
        let names = names_of(&all);
        assert_eq!(names.len(), 18);
        assert!(names.contains(&"sftp_upload"));
        assert!(names.contains(&"sftp_download"));
        assert!(names.contains(&"terminal_open"));
        assert!(names.contains(&"terminal_send"));
        assert!(names.contains(&"terminal_read"));
        assert!(names.contains(&"terminal_close"));

        let p = McpPerms {
            ssh_exec: false,
            sftp_transfer: false,
            ..McpPerms::default()
        };
        let filtered = tools_list(&p);
        let names = names_of(&filtered);
        assert!(!names.contains(&"ssh_exec"));
        assert!(!names.contains(&"sftp_upload"));
        assert!(!names.contains(&"sftp_download"));
        assert!(!names.contains(&"sftp_transfer_list"));
        // terminal_* 随 ssh_exec 组一起被过滤
        assert!(!names.contains(&"terminal_open"));
        assert!(!names.contains(&"terminal_send"));
        assert!(!names.contains(&"terminal_read"));
        assert!(!names.contains(&"terminal_close"));
        assert_eq!(names.len(), 10);
    }

    // ---- 轴一 1.3：MCP HTTP 面 E2E（真服务 + 真 JSON-RPC 往返） ----

    /// 起一台真实 MCP 服务（临时库 + 空闲端口）；调用方负责 mgr.stop()
    async fn boot_mcp(store: Arc<Store>) -> (Arc<McpManager>, String) {
        let mgr = McpManager::new();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        mgr.start(
            store.clone(),
            crate::sftp::SftpManagerState::new(store),
            crate::exec::ExecManagerState::new(tokio::runtime::Handle::current()),
            crate::mcp_terminal::McpTerminalState::new(),
            port,
            String::new(), // 测试不启用 token
            McpPerms::default(),
        )
        .await
        .expect("MCP 服务启动失败");
        (mgr, format!("http://127.0.0.1:{port}/mcp"))
    }

    async fn rpc(url: &str, method: &str, params: Value) -> Value {
        let resp = reqwest::Client::new()
            .post(url)
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
            .send()
            .await
            .expect("HTTP 请求失败");
        assert_eq!(resp.status(), 200, "HTTP 状态异常");
        let v: Value = resp.json().await.expect("响应非 JSON");
        if let Some(err) = v.get("error") {
            panic!("RPC {method} 返回错误: {err}");
        }
        v["result"].clone()
    }

    /// tools/call 的文本载荷与 isError 标志
    async fn call_text(url: &str, name: &str, args: Value) -> (String, bool) {
        let r = rpc(
            url,
            "tools/call",
            json!({ "name": name, "arguments": args }),
        )
        .await;
        (
            r["content"][0]["text"].as_str().unwrap_or("").to_string(),
            r["isError"].as_bool().unwrap_or(false),
        )
    }

    #[tokio::test]
    async fn e2e_http_initialize_and_tools_list() {
        let dir = std::env::temp_dir().join(format!("myssh-mcp-e2e-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        let (mgr, url) = boot_mcp(store).await;

        let init = rpc(&url, "initialize", json!({})).await;
        assert_eq!(init["serverInfo"]["name"], "myssh");
        let tools = rpc(&url, "tools/list", json!({})).await;
        assert_eq!(tools["tools"].as_array().unwrap().len(), 18);

        mgr.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// ssh_exec / terminal_* 真机往返（CI Windows + OpenSSH.Server；vault 为 DPAPI，
    /// 仅 Windows 可跑）。目标主机密钥须已学入 %LOCALAPPDATA%/myssh/known_hosts
    /// （MCP 面无弹窗通路，host key fail-closed）——CI 步骤用 ssh-keyscan 预置。
    #[tokio::test]
    async fn e2e_ssh_exec_and_terminal_roundtrip() {
        let Ok(host) = std::env::var("MYSSH_E2E_HOST") else {
            return;
        };
        let port: u16 = std::env::var("MYSSH_E2E_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22);
        let user = std::env::var("MYSSH_E2E_USER").unwrap_or_else(|_| "tester".into());
        let password = std::env::var("MYSSH_E2E_PASSWORD").expect("MYSSH_E2E_PASSWORD 未设置");

        let dir = std::env::temp_dir().join(format!("myssh-mcp-e2e-ssh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Arc::new(Store::open(&dir.join("t.db")).await.expect("开库失败"));
        let rec = core_store::SessionRecord {
            id: "e2e".into(),
            name: "e2e".into(),
            kind: core_store::SessionKind::Ssh,
            host,
            port,
            user,
            auth_type: core_store::AuthType::Password,
            key_path: None,
            shell: None,
            workdir: None,
            jump_chain: vec![],
            group_path: String::new(),
            color: None,
            encoding: "utf-8".into(),
            su_user: None,
            login_macro: None,
            mcp_perms: Default::default(),
            tags: vec![],
            command: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        store.sessions().upsert(&rec).await.unwrap();
        store
            .credentials()
            .put(
                "e2e",
                core_store::CredentialKind::Password,
                &core_store::Secret::new(password.into_bytes()),
            )
            .await
            .unwrap();

        let (mgr, url) = boot_mcp(store).await;

        // ssh_exec 往返
        let (text, is_err) = call_text(
            &url,
            "ssh_exec",
            json!({ "session_id": "e2e", "command": "echo mcp-e2e-ok" }),
        )
        .await;
        assert!(!is_err, "ssh_exec 报错: {text}");
        assert!(
            text.contains("mcp-e2e-ok"),
            "ssh_exec 输出缺 marker: {text}"
        );

        // terminal_open → send → read → close 往返
        let (text, is_err) = call_text(&url, "terminal_open", json!({ "session_id": "e2e" })).await;
        assert!(!is_err, "terminal_open 报错: {text}");
        let term_id = serde_json::from_str::<Value>(&text).unwrap()["terminalId"]
            .as_str()
            .expect("terminal_open 未返回 terminalId")
            .to_string();

        // 读首轮（banner/提示符）拿到 offset 游标
        let (text, is_err) = call_text(
            &url,
            "terminal_read",
            json!({ "terminalId": term_id, "offset": 0, "wait_seconds": 5 }),
        )
        .await;
        assert!(!is_err, "terminal_read 报错: {text}");
        let mut offset = serde_json::from_str::<Value>(&text).unwrap()["nextOffset"]
            .as_u64()
            .expect("terminal_read 未返回 nextOffset");

        let (_, is_err) = call_text(
            &url,
            "terminal_send",
            json!({ "terminalId": term_id, "data": "echo term-e2e-ok\r" }),
        )
        .await;
        assert!(!is_err, "terminal_send 报错");

        let mut echoed = String::new();
        for _ in 0..10 {
            let (text, is_err) = call_text(
                &url,
                "terminal_read",
                json!({ "terminalId": term_id, "offset": offset, "wait_seconds": 5 }),
            )
            .await;
            assert!(!is_err, "terminal_read 报错: {text}");
            let snap = serde_json::from_str::<Value>(&text).unwrap();
            offset = snap["nextOffset"].as_u64().unwrap();
            echoed.push_str(snap["data"].as_str().unwrap_or(""));
            if echoed.contains("term-e2e-ok") {
                break;
            }
        }
        assert!(
            echoed.contains("term-e2e-ok"),
            "终端输出缺 marker: {echoed:?}"
        );

        let (_, is_err) = call_text(&url, "terminal_close", json!({ "terminalId": term_id })).await;
        assert!(!is_err, "terminal_close 报错");

        mgr.stop().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}
