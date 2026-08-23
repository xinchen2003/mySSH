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
#[derive(Debug, Deserialize)]
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TermOpenSpec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthSpec,
    /// 终端类型，默认 xterm-256color
    pub term: Option<String>,
    /// 启动命令；None = 登录 shell
    pub command: Option<String>,
}

struct TermSession {
    /// 重连时整枚替换（sessions 锁内 swap）
    writer: Arc<PtyWriter>,
    credits: Arc<Semaphore>,
    outstanding: Arc<AtomicI64>,
    /// 最新终端尺寸：重连开 PTY 用（resize 命令实时更新）
    cols: AtomicU64,
    rows: AtomicU64,
    task: tauri::async_runtime::JoinHandle<()>,
}

/// 重连上限与退避：1/2/4/8/16s，5 次后判死
const MAX_RECONNECT_ATTEMPTS: u32 = 5;
fn reconnect_backoff(attempt: u32) -> Duration {
    Duration::from_secs(1u64 << (attempt - 1).min(4))
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
    data: Channel<Response>,
    events: Channel<Value>,
    cols: u32,
    rows: u32,
    state: tauri::State<'_, Arc<TerminalManager>>,
    sessions: tauri::State<'_, Arc<crate::sessions::SessionManagerState>>,
) -> Result<Value, String> {
    let mgr = state.inner().clone();
    let tab_id = next_id("t", &TAB_SEQ);

    // 二选一：内联 spec（临时连接）或 sessionId（存储档案解析）
    let spec = match (spec, session_id) {
        (Some(s), None) => s,
        (None, Some(id)) => crate::sessions::resolve_session_spec(&sessions.store, &id).await?,
        _ => return Err("term_open 需要且仅需 spec 或 sessionId 之一".into()),
    };

    let auth = match spec.auth {
        AuthSpec::Password { password } => AuthMethod::Password(Zeroizing::new(password)),
        AuthSpec::PublicKey {
            key_pem,
            passphrase,
        } => AuthMethod::PublicKey {
            key_pem: Zeroizing::new(key_pem),
            passphrase: passphrase.map(Zeroizing::new),
        },
        AuthSpec::KeyboardInteractive => AuthMethod::KeyboardInteractive,
        AuthSpec::Agent => AuthMethod::Agent,
    };

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
        opts,
        term,
        command: spec.command.clone(),
        data,
        events: events.clone(),
        credits: credits.clone(),
        outstanding: outstanding.clone(),
        reader,
    }));

    mgr.sessions.lock().insert(
        tab_id.clone(),
        TermSession {
            writer: Arc::new(writer),
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

/// 会话监督器：读循环 → 意外断开则指数退避重连（同 events 决策桥仍可用）。
/// 用户 term_close（表项摘除）或重连 5 次皆败 → 终态 closed。
struct SuperviseCtx {
    tab_id: String,
    mgr: Arc<TerminalManager>,
    opts: ConnectOptions,
    term: String,
    command: Option<String>,
    data: Channel<Response>,
    events: Channel<Value>,
    credits: Arc<Semaphore>,
    outstanding: Arc<AtomicI64>,
    reader: PtyReader,
}

async fn supervise(mut ctx: SuperviseCtx) {
    loop {
        read_loop(&mut ctx.reader, &ctx.data, &ctx.credits, &ctx.outstanding).await;

        // 用户主动关闭：表项已被 term_close 摘除 → 静默退出
        if !ctx.mgr.sessions.lock().contains_key(&ctx.tab_id) {
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
                return;
            }
        };
        // 换写半（输入路径无感）；表项中途被摘则放弃
        {
            let mut sessions = ctx.mgr.sessions.lock();
            match sessions.get_mut(&ctx.tab_id) {
                Some(s) => s.writer = Arc::new(writer),
                None => return,
            }
        }
        let _ = ctx.events.send(json!({
            "v": 1, "type": "session_state",
            "tabId": ctx.tab_id, "state": "connected", "reconnected": true,
        }));
        ctx.reader = reader;
    }
}

/// 指数退避重连；用户关闭（表项消失）立即放弃
async fn reconnect(ctx: &SuperviseCtx) -> Option<(PtyReader, PtyWriter)> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        if attempt > MAX_RECONNECT_ATTEMPTS {
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
        let Ok(conn) = SshConnection::connect(ctx.opts.clone()).await else {
            continue;
        };
        if let Ok(pty) = conn
            .open_pty(&ctx.term, cols, rows, ctx.command.as_deref())
            .await
        {
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
    let writer = {
        let sessions = state.sessions.lock();
        sessions.get(&tab_id).map(|s| s.writer.clone())
    };
    match writer {
        Some(w) => w.write(&bytes).await.map_err(|e| e.to_string()),
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
) -> Result<(), String> {
    let session = state.sessions.lock().remove(&tab_id);
    if let Some(session) = session {
        let _ = session.writer.close().await;
        session.task.abort();
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
    reader: &mut PtyReader,
    data_ch: &Channel<Response>,
    credits: &Arc<Semaphore>,
    outstanding: &Arc<AtomicI64>,
) {
    let mut agg: Vec<u8> = Vec::with_capacity(AGG_CAP);
    let mut flush_at = Instant::now() + AGG_WINDOW;

    loop {
        let delay = tokio::time::sleep_until(tokio::time::Instant::from_std(flush_at));
        tokio::pin!(delay);
        tokio::select! {
            msg = reader.next_data() => {
                match msg {
                    Some(bytes) => {
                        agg.extend_from_slice(&bytes);
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
