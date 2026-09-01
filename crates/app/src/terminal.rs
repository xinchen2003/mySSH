//! 终端会话管理：tab 生命周期、8ms/256KB 聚合推送、信用背压、
//! hostkey/keyboard-interactive 决策桥（GUI 弹窗 ↔ russh 回调）。
//!
//! 数据通路规则（规格书第 1/2/6 条 + spike 验证）：
//! - 终端输出只走 `Channel<Response>` 原始二进制，8ms 或 256KB 聚合；
//! - 信用高水位 8MB：flush 前 acquire+forget（permit drop 即归还，闸门会失效——踩坑 #3）；
//! - 输入零聚合直发；
//! - 控制/事件走独立 events Channel（JSON）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, Response};
use tokio::sync::{oneshot, Semaphore};
use zeroize::Zeroizing;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, HostKeyDecision, HostKeyPrompt,
    KeepaliveConfig, KiChallenge, KnownHostsPolicy, PtyReader, PtyWriter, SshConnection,
};

/// 输出聚合时间窗（规格书第 2 条）
const AGG_WINDOW: Duration = Duration::from_millis(8);
/// 单次推送上限（规格书第 2 条）
const AGG_CAP: usize = 256 * 1024;
/// 前端未确认字节数上限（信用背压；超出即停止从 SSH 读取）
const CREDIT_HIGH: u32 = 8 * 1024 * 1024;
/// 弹窗等待上限：超时按拒绝/取消处理，避免悬挂连接
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

/// 前端传入的认证材料（secret 只在内存停留，Zeroizing 落 core-ssh）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AuthSpec {
    Password {
        password: String,
    },
    /// keyPem：OpenSSH/PKCS8/PKCS5/PuTTY .ppk 均可
    PublicKey {
        key_pem: String,
        passphrase: Option<String>,
    },
    KeyboardInteractive,
    Agent,
}

/// 一跳跳板（已解析的认证材料；由 sessions.rs 从档案+保险库解析注入）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JumpHopSpec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthSpec,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TermOpenSpec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthSpec,
    /// ProxyJump 链（就近→最远）；空 = 直连
    #[serde(default)]
    pub jump_chain: Vec<JumpHopSpec>,
    /// 终端类型，默认 xterm-256color
    pub term: Option<String>,
    /// 启动命令；None = 登录 shell
    pub command: Option<String>,
    /// 终端编码（encoding_rs 标签）；默认 utf-8 = 直通不转码
    #[serde(default = "default_encoding")]
    pub encoding: String,
}

fn default_encoding() -> String {
    "utf-8".into()
}

/// 会话编码生效值：term_open 显式入参优先，其次解析结果（档案/内联 spec），最后 utf-8
fn effective_encoding(
    explicit: Option<&str>,
    resolved: &str,
) -> Option<&'static encoding_rs::Encoding> {
    let name = explicit
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(resolved);
    crate::encoding::lookup(name)
}

/// AuthSpec → core-ssh 认证材料（Zeroizing 包裹秘密）
pub(crate) fn auth_method_from(auth: &AuthSpec) -> AuthMethod {
    match auth {
        AuthSpec::Password { password } => AuthMethod::Password(Zeroizing::new(password.clone())),
        AuthSpec::PublicKey {
            key_pem,
            passphrase,
        } => AuthMethod::PublicKey {
            key_pem: Zeroizing::new(key_pem.clone()),
            passphrase: passphrase.clone().map(Zeroizing::new),
        },
        AuthSpec::KeyboardInteractive => AuthMethod::KeyboardInteractive,
        AuthSpec::Agent => AuthMethod::Agent,
    }
}

/// 跳板链 → core-ssh（KI 在跳板上同样弹窗——复用同一决策桥）
pub(crate) fn jump_chain_from(chain: &[JumpHopSpec]) -> Vec<core_ssh::JumpHop> {
    chain
        .iter()
        .map(|h| core_ssh::JumpHop {
            host: h.host.clone(),
            port: h.port,
            user: h.user.clone(),
            auth: auth_method_from(&h.auth),
        })
        .collect()
}
/// 读半抽象：SSH 通道 或 本地 PTY（批次十四 本地会话）
enum AnyReader {
    Ssh(PtyReader),
    Local(crate::local_pty::LocalReader),
}

impl AnyReader {
    async fn next_data(&mut self) -> Option<bytes::Bytes> {
        match self {
            Self::Ssh(r) => r.next_data().await,
            Self::Local(r) => r.next_data().await,
        }
    }
}

/// 写半抽象：同上；统一 term_input/term_resize/term_close 的调用面
enum AnyWriter {
    Ssh(PtyWriter),
    Local(crate::local_pty::LocalWriter),
}

impl AnyWriter {
    async fn write(&self, data: &[u8]) -> Result<(), String> {
        match self {
            Self::Ssh(w) => w.write(data).await.map_err(|e| e.to_string()),
            Self::Local(w) => w.write(data).await,
        }
    }

    async fn resize(&self, cols: u32, rows: u32) -> Result<(), String> {
        match self {
            Self::Ssh(w) => w.resize(cols, rows).await.map_err(|e| e.to_string()),
            Self::Local(w) => w.resize(cols, rows).await,
        }
    }

    async fn close(&self) -> Result<(), String> {
        match self {
            Self::Ssh(w) => w.close().await.map_err(|e| e.to_string()),
            Self::Local(w) => w.close().await,
        }
    }
}

/// 重连语义的后端分支：SSH 持连接参数可重连；本地进程退出即终态
enum Backend {
    Ssh(Box<SshReconnect>),
    Local,
}
/// SSH 重连所需的连接参数（Box 收敛 Backend 两变体体积差）
struct SshReconnect {
    opts: ConnectOptions,
    term: String,
    command: Option<String>,
}

struct TermSession {
    /// 重连时整枚替换（sessions 锁内 swap）
    writer: Arc<AnyWriter>,
    /// 来源会话档案 id（内联 spec 连接为 None）：随会话隧道的归属键
    session_id: Option<String>,
    /// 输入转码器（非 utf-8 会话）：UTF-8 → 目标编码；None = 直通零拷贝
    input_enc: Option<Arc<Mutex<crate::encoding::InputEncoder>>>,
    credits: Arc<Semaphore>,
    outstanding: Arc<AtomicI64>,
    /// 最新终端尺寸：重连开 PTY 用（resize 命令实时更新）
    cols: AtomicU64,
    rows: AtomicU64,
    task: tauri::async_runtime::JoinHandle<()>,
}

/// 重连退避：1/2/4/8/16s 封顶
fn reconnect_backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << (attempt - 1).min(4))
}

/// 重连次数上限：读 settings KV（terminal.reconnectAttempts，前端设置项 0-20，默认 5）。
/// 每个重连周期读一次（运行中改设置即刻生效）；读不到/非法值回退默认，clamp 0-20。
const DEFAULT_RECONNECT_ATTEMPTS: u32 = 5;
const MAX_RECONNECT_ATTEMPTS: u32 = 20;
async fn reconnect_attempts(store: &core_store::Store) -> u32 {
    let raw = store
        .settings()
        .get("terminal.reconnectAttempts")
        .await
        .ok()
        .flatten();
    parse_reconnect_attempts(raw.as_deref())
}

/// 设置值解析：JSON 数值，clamp 0-20；缺失/非法一律回退默认
fn parse_reconnect_attempts(raw: Option<&str>) -> u32 {
    raw.and_then(|v| serde_json::from_str::<u64>(v).ok())
        .map(|n| n.min(MAX_RECONNECT_ATTEMPTS as u64) as u32)
        .unwrap_or(DEFAULT_RECONNECT_ATTEMPTS)
}

/// 全局终端管理器。Tauri 以 `Arc<TerminalManager>` 托管，
/// 决策桥闭包与读取任务均持 Arc 引用。
#[derive(Default)]
pub struct TerminalManager {
    sessions: Mutex<HashMap<String, TermSession>>,
    /// hostkey 决策待决表：confirmId → 回调通道
    hostkey_confirms: Mutex<HashMap<String, oneshot::Sender<HostKeyDecision>>>,
    /// KI 应答待决表：confirmId → 回调通道（None = 用户取消）
    ki_confirms: Mutex<HashMap<String, oneshot::Sender<Option<Vec<String>>>>>,
}

static TAB_SEQ: AtomicU64 = AtomicU64::new(1);
static CONFIRM_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str, seq: &AtomicU64) -> String {
    format!("{prefix}{}", seq.fetch_add(1, Ordering::Relaxed))
}

pub(crate) fn known_hosts_path() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("myssh")
        .join("known_hosts")
}

// Tauri 命令的 State 参数不占真实调用签名；clippy 误伤，豁免
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn term_open(
    spec: Option<TermOpenSpec>,
    session_id: Option<String>,
    // 前端显式指定的终端编码（档案会话可覆盖记录值）；空/缺省 = 取解析结果
    encoding: Option<String>,
    data: Channel<Response>,
    events: Channel<Value>,
    cols: u32,
    rows: u32,
    state: tauri::State<'_, Arc<TerminalManager>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
    tunnels_state: tauri::State<'_, Arc<crate::tunnels::TunnelManagerState>>,
) -> Result<Value, String> {
    let mgr = state.inner().clone();
    let tab_id = next_id("t", &TAB_SEQ);

    // 二选一：内联 spec（临时连接）或 sessionId（存储档案解析；可能是本地会话）
    let via_session = session_id.clone();
    let target = match (spec, session_id) {
        (Some(s), None) => crate::sessions::ResolvedTarget::Ssh(s),
        (None, Some(id)) => crate::sessions::resolve_session_target(&sessions.store, &id).await?,
        _ => return Err("term_open 需要且仅需 spec 或 sessionId 之一".into()),
    };
    let spec = match target {
        crate::sessions::ResolvedTarget::Ssh(s) => s,
        crate::sessions::ResolvedTarget::Local(ls) => {
            // 本地会话：ConPTY 直连读循环；无 SSH 连接/hostkey/KI/随会话隧道
            // ConPTY 协议面恒为 UTF-8（控制台代码页由 ConPTY 内部转译），
            // 任何转码都会把 UTF-8 流毁成乱码——忽略显式入参与档案值，恒直通
            let out_encoding = None;
            let input_enc =
                out_encoding.map(|e| Arc::new(Mutex::new(crate::encoding::InputEncoder::new(e))));
            let pty = tauri::async_runtime::spawn_blocking(move || {
                crate::local_pty::spawn(&ls, cols, rows)
            })
            .await
            .map_err(|e| format!("本地终端任务失败: {e}"))??;
            let shell = pty.shell.clone();
            let credits = Arc::new(Semaphore::new(CREDIT_HIGH as usize));
            let outstanding = Arc::new(AtomicI64::new(0));
            let task = tauri::async_runtime::spawn(supervise(SuperviseCtx {
                tab_id: tab_id.clone(),
                mgr: mgr.clone(),
                session_id: via_session.clone(),
                tunnel_mgr: tunnels_state.mgr.clone(),
                store: sessions.store.clone(),
                backend: Backend::Local,
                out_encoding,
                data,
                events: events.clone(),
                credits: credits.clone(),
                outstanding: outstanding.clone(),
                reader: AnyReader::Local(pty.reader),
            }));
            mgr.sessions.lock().insert(
                tab_id.clone(),
                TermSession {
                    writer: Arc::new(AnyWriter::Local(pty.writer)),
                    session_id: via_session,
                    input_enc,
                    credits,
                    outstanding,
                    cols: AtomicU64::new(cols as u64),
                    rows: AtomicU64::new(rows as u64),
                    task,
                },
            );
            let _ = events.send(json!({
                "v": 1, "type": "session_state", "tabId": tab_id, "state": "connected",
                "kind": "local", "shell": shell,
            }));
            return Ok(json!({ "tabId": tab_id }));
        }
    };

    let auth = auth_method_from(&spec.auth);
    let jump_chain = jump_chain_from(&spec.jump_chain);
    // 终端编码：非 utf-8 时输出流式 decode → UTF-8，输入 UTF-8 → 目标编码
    let out_encoding = effective_encoding(encoding.as_deref(), &spec.encoding);
    let input_enc =
        out_encoding.map(|e| Arc::new(Mutex::new(crate::encoding::InputEncoder::new(e))));

    // hostkey 决策桥：prompter 经 events 发弹窗帧，oneshot 等 hostkey_confirm 命令
    let hk_events = events.clone();
    let hk_mgr = mgr.clone();
    let hostkey_prompter = move |prompt: HostKeyPrompt| {
        let confirm_id = next_id("hk", &CONFIRM_SEQ);
        let frame = match &prompt {
            HostKeyPrompt::Unknown {
                host,
                port,
                key_type,
                fingerprint,
            } => json!({
                "v": 1, "type": "hostkey_prompt", "confirmId": confirm_id,
                "kind": "unknown", "host": host, "port": port,
                "keyType": key_type, "fingerprint": fingerprint,
            }),
            HostKeyPrompt::Changed {
                host,
                port,
                key_type,
                old_fingerprint,
                new_fingerprint,
            } => json!({
                "v": 1, "type": "hostkey_prompt", "confirmId": confirm_id,
                "kind": "changed", "host": host, "port": port,
                "keyType": key_type, "oldFingerprint": old_fingerprint,
                "newFingerprint": new_fingerprint,
            }),
        };
        let _ = hk_events.send(frame);
        let (tx, rx) = oneshot::channel();
        hk_mgr.hostkey_confirms.lock().insert(confirm_id, tx);
        async move {
            match tokio::time::timeout(CONFIRM_TIMEOUT, rx).await {
                Ok(Ok(decision)) => decision,
                // 超时或发送端消失（窗口关闭）：fail-closed 拒绝
                _ => HostKeyDecision::Reject,
            }
        }
    };

    // KI 应答桥：同上，走 ki_respond 命令
    let ki_events = events.clone();
    let ki_mgr = mgr.clone();
    let ki_prompter = Arc::new(move |challenge: KiChallenge| {
        let confirm_id = next_id("ki", &CONFIRM_SEQ);
        let _ = ki_events.send(json!({
            "v": 1, "type": "ki_challenge", "confirmId": confirm_id,
            "name": challenge.name, "instruction": challenge.instruction,
            "prompts": challenge.prompts.iter()
                .map(|p| json!({ "prompt": p.prompt, "echo": p.echo }))
                .collect::<Vec<_>>(),
        }));
        let (tx, rx) = oneshot::channel();
        ki_mgr.ki_confirms.lock().insert(confirm_id, tx);
        async move {
            match tokio::time::timeout(CONFIRM_TIMEOUT, rx).await {
                Ok(Ok(answers)) => answers,
                _ => None,
            }
        }
    });

    let opts = ConnectOptions {
        host: spec.host.clone(),
        port: spec.port,
        user: spec.user.clone(),
        auth,
        jump_chain,
        class: ConnClass::Interactive,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        host_key_check: HostKeyCheck::KnownHosts(KnownHostsPolicy {
            path: known_hosts_path(),
            prompter: Arc::new(hostkey_prompter),
        }),
        ki_prompter: Some(ki_prompter),
    };

    let conn = SshConnection::connect(opts.clone())
        .await
        .map_err(|e| e.to_string())?;
    // 随会话自动建立的隧道（规格书 M2）；fire-and-forget，失败仅日志
    if let Some(sid) = via_session.clone() {
        let tmgr = tunnels_state.mgr.clone();
        let store = sessions.store.clone();
        let tunnel_events = events.clone();
        tauri::async_runtime::spawn(async move {
            crate::tunnels::start_session_tunnels(tmgr, store, sid, tunnel_events).await;
        });
    }
    let term = spec.term.clone().unwrap_or_else(|| "xterm-256color".into());
    let pty = conn
        .open_pty(&term, cols, rows, spec.command.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let (reader, writer) = pty.split();

    let credits = Arc::new(Semaphore::new(CREDIT_HIGH as usize));
    let outstanding = Arc::new(AtomicI64::new(0));
    let task = tauri::async_runtime::spawn(supervise(SuperviseCtx {
        tab_id: tab_id.clone(),
        mgr: mgr.clone(),
        session_id: via_session.clone(),
        tunnel_mgr: tunnels_state.mgr.clone(),
        store: sessions.store.clone(),
        backend: Backend::Ssh(Box::new(SshReconnect {
            opts,
            term,
            command: spec.command.clone(),
        })),
        out_encoding,
        data,
        events: events.clone(),
        credits: credits.clone(),
        outstanding: outstanding.clone(),
        reader: AnyReader::Ssh(reader),
    }));

    mgr.sessions.lock().insert(
        tab_id.clone(),
        TermSession {
            writer: Arc::new(AnyWriter::Ssh(writer)),
            session_id: via_session,
            input_enc,
            credits,
            outstanding,
            cols: AtomicU64::new(cols as u64),
            rows: AtomicU64::new(rows as u64),
            task,
        },
    );

    let _ = events.send(json!({
        "v": 1, "type": "session_state", "tabId": tab_id, "state": "connected",
        "host": spec.host, "port": spec.port, "user": spec.user,
    }));
    Ok(json!({ "tabId": tab_id }))
}

/// 同会话最后一个终端消失时停止其随会话隧道（§9.2：服务器断开后停止）
fn stop_session_tunnels_if_last(ctx: &SuperviseCtx) {
    let Some(sid) = ctx.session_id.clone() else {
        return;
    };
    let still_open = ctx
        .mgr
        .sessions
        .lock()
        .values()
        .any(|s| s.session_id.as_deref() == Some(sid.as_str()));
    if still_open {
        return;
    }
    let tmgr = ctx.tunnel_mgr.clone();
    let store = ctx.store.clone();
    tauri::async_runtime::spawn(async move {
        crate::tunnels::stop_session_tunnels(tmgr, store, sid).await;
    });
}

/// 会话监督器：读循环 → 意外断开则指数退避重连（同 events 决策桥仍可用）。
/// 用户 term_close（表项摘除）或重连次数耗尽（terminal.reconnectAttempts，默认 5） → 终态 closed。
struct SuperviseCtx {
    tab_id: String,
    mgr: Arc<TerminalManager>,
    /// 随会话隧道停止所需的归属与句柄（session_id 为 None 时短路）
    session_id: Option<String>,
    tunnel_mgr: Arc<core_tunnel::TunnelManager>,
    store: Arc<core_store::Store>,
    backend: Backend,
    /// 输出转码（非 utf-8 会话）：读循环内建 Decoder，重连即重建
    out_encoding: Option<&'static encoding_rs::Encoding>,
    data: Channel<Response>,
    events: Channel<Value>,
    credits: Arc<Semaphore>,
    outstanding: Arc<AtomicI64>,
    reader: AnyReader,
}

async fn supervise(mut ctx: SuperviseCtx) {
    loop {
        read_loop(
            &mut ctx.reader,
            &ctx.data,
            &ctx.credits,
            &ctx.outstanding,
            ctx.out_encoding,
        )
        .await;

        // 用户主动关闭：表项已被 term_close 摘除 → 静默退出
        if !ctx.mgr.sessions.lock().contains_key(&ctx.tab_id) {
            return;
        }

        // 本地 PTY：进程退出即终态 closed——exit 是用户意图，不做自动重开
        if matches!(ctx.backend, Backend::Local) {
            let _ = ctx.events.send(json!({
                "v": 1, "type": "session_state",
                "tabId": ctx.tab_id, "state": "closed",
            }));
            ctx.mgr.sessions.lock().remove(&ctx.tab_id);
            stop_session_tunnels_if_last(&ctx);
            return;
        }

        let (reader, writer) = match reconnect(&ctx).await {
            Some(pair) => pair,
            None => {
                // 重连耗尽：终态 closed + 摘除表项（任务即表项持有者，自生自灭）
                let _ = ctx.events.send(json!({
                    "v": 1, "type": "session_state",
                    "tabId": ctx.tab_id, "state": "closed",
                }));
                ctx.mgr.sessions.lock().remove(&ctx.tab_id);
                stop_session_tunnels_if_last(&ctx);
                return;
            }
        };
        // 换写半（输入路径无感）；表项中途被摘则放弃
        {
            let mut sessions = ctx.mgr.sessions.lock();
            match sessions.get_mut(&ctx.tab_id) {
                Some(s) => s.writer = Arc::new(AnyWriter::Ssh(writer)),
                None => return,
            }
        }
        let _ = ctx.events.send(json!({
            "v": 1, "type": "session_state",
            "tabId": ctx.tab_id, "state": "connected", "reconnected": true,
        }));
        ctx.reader = AnyReader::Ssh(reader);
    }
}

/// 指数退避重连；用户关闭（表项消失）立即放弃
async fn reconnect(ctx: &SuperviseCtx) -> Option<(PtyReader, PtyWriter)> {
    let Backend::Ssh(rc) = &ctx.backend else {
        return None; // 本地会话不进重连路径（supervise 已短路）
    };
    let (opts, term, command) = (&rc.opts, &rc.term, &rc.command);
    let max_attempts = reconnect_attempts(&ctx.store).await;
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if attempt > max_attempts {
            return None;
        }
        let _ = ctx.events.send(json!({
            "v": 1, "type": "session_state",
            "tabId": ctx.tab_id, "state": "reconnecting", "attempt": attempt,
        }));
        tokio::time::sleep(reconnect_backoff(attempt)).await;
        if !ctx.mgr.sessions.lock().contains_key(&ctx.tab_id) {
            return None;
        }
        let (cols, rows) = {
            let sessions = ctx.mgr.sessions.lock();
            match sessions.get(&ctx.tab_id) {
                Some(s) => (
                    s.cols.load(Ordering::Relaxed) as u32,
                    s.rows.load(Ordering::Relaxed) as u32,
                ),
                None => return None,
            }
        };
        let Ok(conn) = SshConnection::connect(opts.clone()).await else {
            continue;
        };
        if let Ok(pty) = conn.open_pty(term, cols, rows, command.as_deref()).await {
            return Some(pty.split());
        }
    }
}

#[tauri::command]
pub async fn term_input(
    tab_id: String,
    bytes: Vec<u8>,
    state: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    // 输入转码在锁内取走编码器引用（Arc 克隆即释放锁），utf-8（None）零拷贝直通
    let entry = {
        let sessions = state.sessions.lock();
        sessions
            .get(&tab_id)
            .map(|s| (s.writer.clone(), s.input_enc.clone()))
    };
    match entry {
        Some((w, input_enc)) => {
            let bytes = match input_enc {
                Some(enc) => enc.lock().encode(&bytes),
                None => bytes,
            };
            w.write(&bytes).await.map_err(|e| e.to_string())
        }
        None => Err(format!("unknown tab {tab_id}")),
    }
}

#[tauri::command]
pub async fn term_credit(
    tab_id: String,
    bytes: u64,
    state: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    let entry = {
        let sessions = state.sessions.lock();
        sessions
            .get(&tab_id)
            .map(|s| (s.credits.clone(), s.outstanding.clone()))
    };
    if let Some((credits, outstanding)) = entry {
        outstanding.fetch_sub(bytes as i64, Ordering::Relaxed);
        credits.add_permits(bytes as usize);
    }
    Ok(())
}

#[tauri::command]
pub async fn term_resize(
    tab_id: String,
    cols: u32,
    rows: u32,
    state: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    let writer = {
        let sessions = state.sessions.lock();
        sessions.get(&tab_id).map(|s| {
            s.cols.store(cols as u64, Ordering::Relaxed);
            s.rows.store(rows as u64, Ordering::Relaxed);
            s.writer.clone()
        })
    };
    match writer {
        Some(w) => w.resize(cols, rows).await.map_err(|e| e.to_string()),
        None => Err(format!("unknown tab {tab_id}")),
    }
}

#[tauri::command]
pub async fn term_close(
    tab_id: String,
    state: tauri::State<'_, Arc<TerminalManager>>,
    tunnels_state: tauri::State<'_, Arc<crate::tunnels::TunnelManagerState>>,
    sessions_state: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<(), String> {
    let session = state.sessions.lock().remove(&tab_id);
    if let Some(session) = session {
        let _ = session.writer.close().await;
        session.task.abort();
        if let Some(sid) = session.session_id {
            let still_open = state
                .sessions
                .lock()
                .values()
                .any(|s| s.session_id.as_deref() == Some(sid.as_str()));
            if !still_open {
                let tmgr = tunnels_state.mgr.clone();
                let store = sessions_state.store.clone();
                tauri::async_runtime::spawn(async move {
                    crate::tunnels::stop_session_tunnels(tmgr, store, sid).await;
                });
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn hostkey_confirm(
    confirm_id: String,
    accept: bool,
    remember: bool,
    state: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    let tx = state.hostkey_confirms.lock().remove(&confirm_id);
    if let Some(tx) = tx {
        let decision = match (accept, remember) {
            (true, true) => HostKeyDecision::Learn,
            (true, false) => HostKeyDecision::AcceptOnce,
            (false, _) => HostKeyDecision::Reject,
        };
        let _ = tx.send(decision);
    }
    Ok(())
}

#[tauri::command]
pub async fn ki_respond(
    confirm_id: String,
    answers: Option<Vec<String>>,
    state: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    let tx = state.ki_confirms.lock().remove(&confirm_id);
    if let Some(tx) = tx {
        let _ = tx.send(answers);
    }
    Ok(())
}

/// 终端读取循环：8ms/256KB 聚合 + 信用背压（spike 验证形态）。
/// 信用耗尽即停止 next_data() → russh 不再确认窗口 → 服务端停发，内存不堆积。
/// EOF/Close 时冲净残余即返回——断线语义与重连由 supervise() 负责。
async fn read_loop(
    reader: &mut AnyReader,
    data_ch: &Channel<Response>,
    credits: &Arc<Semaphore>,
    outstanding: &Arc<AtomicI64>,
    out_encoding: Option<&'static encoding_rs::Encoding>,
) {
    let mut agg: Vec<u8> = Vec::with_capacity(AGG_CAP);
    let mut flush_at = Instant::now() + AGG_WINDOW;
    // 非 utf-8：流式 Decoder（跨帧半字符内部缓冲），decode 产物进既有聚合通路；
    // utf-8（None）：完全直通，不引入任何拷贝
    let mut decoder = out_encoding.map(crate::encoding::OutputDecoder::new);

    loop {
        let delay = tokio::time::sleep_until(tokio::time::Instant::from_std(flush_at));
        tokio::pin!(delay);
        tokio::select! {
            msg = reader.next_data() => {
                match msg {
                    Some(bytes) => {
                        match decoder.as_mut() {
                            Some(dec) => dec.decode_append(&bytes, &mut agg),
                            None => agg.extend_from_slice(&bytes),
                        }
                        if agg.len() >= AGG_CAP {
                            flush(data_ch, &mut agg, credits, outstanding).await;
                            flush_at = Instant::now() + AGG_WINDOW;
                        }
                    }
                    None => {
                        flush(data_ch, &mut agg, credits, outstanding).await;
                        return;
                    }
                }
            }
            _ = &mut delay => {
                if !agg.is_empty() {
                    flush(data_ch, &mut agg, credits, outstanding).await;
                }
                flush_at = Instant::now() + AGG_WINDOW;
            }
        }
    }
}

async fn flush(
    data_ch: &Channel<Response>,
    agg: &mut Vec<u8>,
    credits: &Arc<Semaphore>,
    outstanding: &Arc<AtomicI64>,
) {
    if agg.is_empty() {
        return;
    }
    let buf = std::mem::replace(agg, Vec::with_capacity(AGG_CAP));
    // 等待前端信用——背压点；等待期间读取循环挂起。
    // permit 必须 forget，否则 drop 即归还，闸门形同虚设（实测踩中）。
    match credits.acquire_many(buf.len() as u32).await {
        Ok(permit) => permit.forget(),
        Err(_) => return, // 信号量关闭（会话拆除）：丢弃残余数据
    }
    outstanding.fetch_add(buf.len() as i64, Ordering::Relaxed);
    let _ = data_ch.send(Response::new(buf));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_attempts_parse_fallback_and_clamp() {
        // 缺失/非法 → 默认 5
        assert_eq!(parse_reconnect_attempts(None), 5);
        assert_eq!(parse_reconnect_attempts(Some("null")), 5);
        assert_eq!(parse_reconnect_attempts(Some("\"abc\"")), 5);
        assert_eq!(parse_reconnect_attempts(Some("-1")), 5);
        assert_eq!(parse_reconnect_attempts(Some("3.5")), 5);
        // 正常值
        assert_eq!(parse_reconnect_attempts(Some("0")), 0);
        assert_eq!(parse_reconnect_attempts(Some("7")), 7);
        assert_eq!(parse_reconnect_attempts(Some("20")), 20);
        // 超界 clamp 到 20
        assert_eq!(parse_reconnect_attempts(Some("99")), 20);
    }
}
