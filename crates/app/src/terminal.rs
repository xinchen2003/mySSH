//! 终端会话管理：tab 生命周期、8ms/256KB 聚合推送、信用背压、
//! hostkey/keyboard-interactive 决策桥（GUI 弹窗 ↔ russh 回调）。
//!
//! 数据通路规则（规格书第 1/2/6 条 + spike 验证）：
//! - 终端输出只走 `Channel<Response>` 原始二进制，8ms 或 256KB 聚合；
//! - 信用高水位 8MB + (tabId, streamEpoch) 累计 ACK：flush 前 acquire+forget（permit drop 即
//!   归还，闸门会失效——踩坑 #3）；帧自带代际/序号/offset 头；send 失败该 epoch 断代（C4）；
//! - 输入零聚合直发；
//! - 控制/事件走独立 events Channel（JSON）。
//!
//! 本文件现为命令 facade（卡 2 拆分）：TerminalManager + 9 命令 + hostkey/KI 决策桥；
//! 生命周期（supervise/su/宏）在 terminal/session.rs，热循环在 terminal/stream.rs。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::ipc::{Channel, Response};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use core_ssh::{
    ConnClass, ConnectOptions, HostKeyCheck, HostKeyDecision, HostKeyPrompt, KeepaliveConfig,
    KiChallenge, KnownHostsPolicy, SshConnection,
};

// 连接类型（AuthSpec/TermOpenSpec/JumpHopSpec）与建连 helper 已迁 connect.rs（卡 5）
use crate::connect::{auth_method_from, jump_chain_from, known_hosts_path, TermOpenSpec};
// 帧格式/信用协议已迁 stream_protocol.rs（卡 1）
use crate::stream_protocol::CreditState;

mod session;
mod stream;

use session::{
    macro_lines, supervise, AnyReader, AnyWriter, Backend, SshReconnect, SuConfig, SuperviseCtx,
    TermSession,
};
use stream::{RingBuf, RING_CAP};

/// 弹窗等待上限：超时按拒绝/取消处理，避免悬挂连接
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(120);

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

impl TerminalManager {
    /// PR-0 可观测性：每 tab 的背压快照（outstanding / 可用 credit）
    pub(crate) fn perf_json(&self) -> Value {
        let sessions = self.sessions.lock();
        let tabs: Vec<Value> = sessions
            .iter()
            .map(|(id, s)| {
                json!({
                    "tabId": id,
                    "streamEpoch": s.credit.epoch,
                    "outstandingBytes": s.credit.outstanding(),
                    // PR-17 二期：后台 ring 观测（term.outstanding/term.ring 资源行）
                    "focused": s.focused.load(Ordering::Relaxed),
                    "ringBytes": s.ring.len(),
                    "ringTruncated": s.ring.truncated.load(Ordering::Relaxed),
                    "creditAvailable": s.credit.available_credit(),
                    "streamBroken": s.credit.is_broken(),
                })
            })
            .collect();
        json!({ "tabs": tabs.len(), "sessions": tabs })
    }
}

static TAB_SEQ: AtomicU64 = AtomicU64::new(1);
static CONFIRM_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_id(prefix: &str, seq: &AtomicU64) -> String {
    format!("{prefix}{}", seq.fetch_add(1, Ordering::Relaxed))
}

// Tauri 命令的 State 参数不占真实调用签名；clippy 误伤，豁免
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn term_open(
    spec: Option<TermOpenSpec>,
    session_id: Option<String>,
    // 前端显式指定的终端编码（档案会话可覆盖记录值）；空/缺省 = 取解析结果
    encoding: Option<String>,
    // 数据通道代际标识（前端建 Channel 时生成）：本 tab 信用 ACK 身份的一部分
    stream_epoch: u64,
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
            // 登录宏随 ls 携带；move 进闭包前先取引用
            let ls_login_macro = ls.login_macro.clone();
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
            // 本地会话登录宏：ls 被 move 进闭包前先解析行
            let login_macro = macro_lines(ls_login_macro.as_deref().unwrap_or(""));
            let shell = pty.shell.clone();
            let credit = Arc::new(CreditState::new(stream_epoch));
            let focused = Arc::new(AtomicBool::new(true));
            let ring = Arc::new(RingBuf::new(RING_CAP));
            let writer = Arc::new(AnyWriter::Local(pty.writer));
            let task = tauri::async_runtime::spawn(supervise(SuperviseCtx {
                tab_id: tab_id.clone(),
                mgr: mgr.clone(),
                session_id: via_session.clone(),
                tunnel_mgr: tunnels_state.mgr.clone(),
                store: sessions.store.clone(),
                backend: Backend::Local,
                // 本地会话无 su（Windows ConPTY 无此概念）
                su: None,
                login_macro,
                // 本地 ConPTY 恒 UTF-8 直通（见开处注释）
                input_enc: None,
                writer: writer.clone(),
                out_encoding,
                data,
                events: events.clone(),
                credit: credit.clone(),
                focused: focused.clone(),
                ring: ring.clone(),
                reader: AnyReader::Local(pty.reader),
            }));
            mgr.sessions.lock().insert(
                tab_id.clone(),
                TermSession {
                    writer,
                    session_id: via_session,
                    input_enc,
                    credit,
                    cols: AtomicU64::new(cols as u64),
                    rows: AtomicU64::new(rows as u64),
                    focused,
                    ring,
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
    // su 二级登录：档案配了 su_user 才启用；密码包装 Zeroizing 随连接存活
    let su = spec
        .su_user
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(|u| SuConfig {
            user: u.to_string(),
            password: spec.su_password.clone().map(Zeroizing::new),
        });
    let writer = Arc::new(AnyWriter::Ssh(writer));

    let credit = Arc::new(CreditState::new(stream_epoch));
    let focused = Arc::new(AtomicBool::new(true));
    let ring = Arc::new(RingBuf::new(RING_CAP));
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
        su,
        login_macro: macro_lines(spec.login_macro.as_deref().unwrap_or("")),
        input_enc: input_enc.clone(),
        writer: writer.clone(),
        out_encoding,
        data,
        events: events.clone(),
        credit: credit.clone(),
        focused: focused.clone(),
        ring: ring.clone(),
        reader: AnyReader::Ssh(reader),
    }));

    mgr.sessions.lock().insert(
        tab_id.clone(),
        TermSession {
            writer,
            session_id: via_session,
            input_enc,
            credit,
            cols: AtomicU64::new(cols as u64),
            rows: AtomicU64::new(rows as u64),
            focused,
            ring,
            task,
        },
    );

    let _ = events.send(json!({
        "v": 1, "type": "session_state", "tabId": tab_id, "state": "connected",
        "host": spec.host, "port": spec.port, "user": spec.user,
    }));
    Ok(json!({ "tabId": tab_id }))
}

/// 终端内文件传输（ZMODEM/trzsz）协议帧写入：二进制直发，不经过输入编码器
/// （协议字节非 UTF-8 文本，转码必坏）；b64 载荷避免 JSON 字节数组逐字节膨胀。
#[tauri::command]
pub async fn term_input_raw(
    tab_id: String,
    b64: String,
    state: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("b64 解码失败: {e}"))?;
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
    stream_epoch: u64,
    acked_total: u64,
    state: tauri::State<'_, Arc<TerminalManager>>,
) -> Result<(), String> {
    let credit = {
        let sessions = state.sessions.lock();
        sessions.get(&tab_id).map(|s| s.credit.clone())
    };
    // 累计 ACK：旧 epoch/断代/重复 ACK 由 CreditState 内部丢弃
    if let Some(credit) = credit {
        credit.ack(stream_epoch, acked_total);
    }
    Ok(())
}

/// 前台/后台标记（PR-17 二期）：后台 tab 信用耗尽转 ring buffer；
/// 回前台由读循环检查点触发回放（截断提示先行，C11）
#[tauri::command]
pub async fn term_focus(
    state: tauri::State<'_, Arc<TerminalManager>>,
    tab_id: String,
    focused: bool,
) -> Result<(), String> {
    let entry = state
        .sessions
        .lock()
        .get(&tab_id)
        .map(|s| s.focused.clone());
    if let Some(f) = entry {
        f.store(focused, Ordering::Release);
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
