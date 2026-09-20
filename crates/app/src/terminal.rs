//! 终端会话管理：tab 生命周期、8ms/256KB 聚合推送、信用背压、
//! hostkey/keyboard-interactive 决策桥（GUI 弹窗 ↔ russh 回调）。
//!
//! 数据通路规则（规格书第 1/2/6 条 + spike 验证）：
//! - 终端输出只走 `Channel<Response>` 原始二进制，8ms 或 256KB 聚合；
//! - 信用高水位 8MB + (tabId, streamEpoch) 累计 ACK：flush 前 acquire+forget（permit drop 即
//!   归还，闸门会失效——踩坑 #3）；帧自带代际/序号/offset 头；send 失败该 epoch 断代（C4）；
//! - 输入零聚合直发；
//! - 控制/事件走独立 events Channel（JSON）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

/// 帧头长度：streamEpoch | frameSeq | startOffset | endOffset（各 u64 LE，PR-6/C4）
const FRAME_HEADER_LEN: usize = 32;

/// 单代际（streamEpoch）终端信用状态（PR-6 累计 ACK 协议）：
/// - epoch 随数据 Channel 创建定死（term_open 入参），代际内 sent/acked 单调增；
/// - sent range 登记先于 send（C4）；send 失败即断代——不回滚、不复用 offset、
///   同 epoch 不再发送，失败批次既不回补信用也不接受其 ACK；
/// - ACK 只接受当前 epoch：`incoming <= acked_total` 丢弃；`incoming > sent_total` 钳制。
struct CreditState {
    /// 本代际标识（前端建数据 Channel 时生成，随 term_open 传入）
    epoch: u64,
    /// 已登记发送的字节总量（send 失败也不回滚）
    sent_total: AtomicU64,
    /// 前端已确认消费的字节总量（累计 ACK，单调增）
    acked_total: AtomicU64,
    /// 下一帧序号（单写者：读循环）
    frame_seq: AtomicU64,
    /// send 失败断代标记：本代际不再发送任何数据
    broken: AtomicBool,
    /// 信用闸：可用 permit = 前端还可接收的字节数
    credits: Semaphore,
}

impl CreditState {
    fn new(epoch: u64) -> Self {
        Self {
            epoch,
            sent_total: AtomicU64::new(0),
            acked_total: AtomicU64::new(0),
            frame_seq: AtomicU64::new(0),
            broken: AtomicBool::new(false),
            credits: Semaphore::new(CREDIT_HIGH as usize),
        }
    }

    fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Acquire)
    }

    /// 在途未确认字节数（perf 观测）
    fn outstanding(&self) -> u64 {
        self.sent_total
            .load(Ordering::Acquire)
            .saturating_sub(self.acked_total.load(Ordering::Acquire))
    }

    /// flush 路径（单写者=读循环）：分配 frameSeq/offset → 登记 sent range → 组帧。
    /// None = 已断代或 offset 溢出（溢出同时断代，不回绕）。
    fn alloc_frame(&self, payload: Vec<u8>) -> Option<Vec<u8>> {
        if self.is_broken() {
            return None;
        }
        let start = self.sent_total.load(Ordering::Acquire);
        let Some(end) = start.checked_add(payload.len() as u64) else {
            // u64 边界（实际不可达：8MB 窗口下需 EB 级输出）：断代处理
            self.broken.store(true, Ordering::Release);
            return None;
        };
        let seq = self.frame_seq.fetch_add(1, Ordering::AcqRel);
        // C4 顺序：登记先于 send——send 失败不回滚
        self.sent_total.store(end, Ordering::Release);
        let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + payload.len());
        frame.extend_from_slice(&self.epoch.to_le_bytes());
        frame.extend_from_slice(&seq.to_le_bytes());
        frame.extend_from_slice(&start.to_le_bytes());
        frame.extend_from_slice(&end.to_le_bytes());
        frame.extend_from_slice(&payload);
        Some(frame)
    }

    /// send 失败：断代（C4：不回滚、不复用 offset、不允许同 epoch 继续发送）
    fn mark_broken(&self) {
        self.broken.store(true, Ordering::Release);
    }

    /// 累计 ACK（term_credit 路径）：返回新增信用字节数；0 = 忽略。
    /// 旧 epoch / 断代 / 重复或倒退 ACK 一律丢弃；incoming > sent 钳制并记协议异常。
    fn ack(&self, epoch: u64, incoming: u64) -> u64 {
        if epoch != self.epoch || self.is_broken() {
            return 0;
        }
        let mut cur = self.acked_total.load(Ordering::Acquire);
        loop {
            if incoming <= cur {
                return 0;
            }
            let sent = self.sent_total.load(Ordering::Acquire);
            if incoming > sent {
                tracing::warn!(epoch, incoming, sent, "终端 ACK 超过已发送量，按钳制处理");
            }
            let newly = (incoming - cur).min(sent.saturating_sub(cur));
            if newly == 0 {
                return 0;
            }
            match self.acked_total.compare_exchange_weak(
                cur,
                cur + newly,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.credits.add_permits(newly as usize);
                    return newly;
                }
                Err(actual) => cur = actual,
            }
        }
    }
}

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
    /// 登录后切换用户（su）目标用户名；None/空 = 不切换（批次二十二）
    #[serde(default)]
    pub su_user: Option<String>,
    /// su 密码（内存经手即弃；档案路径由 resolve 从保险库读出）
    #[serde(default)]
    pub su_password: Option<String>,
    /// 登录宏：进 shell 后自动逐行执行的命令（多行文本）；None/空 = 不执行。
    /// 语义：无 su 即发；有 su 则密码应答后发；配 command 的会话不执行；重连重放
    #[serde(default)]
    pub login_macro: Option<String>,
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

/// su 二级登录配置（批次二十二）：登录 shell 起来后自动 `su - <user>`，
/// 配了密码则在密码提示出现时自动应答一次（绝不重试——答错交回用户手输，
/// 避免错误密码反复触发锁定策略）
struct SuConfig {
    user: String,
    password: Option<Zeroizing<String>>,
}

/// 读循环内的一次性密码 expect：deadline 后或应答后即失效
struct SuWatch {
    password: Option<Zeroizing<String>>,
    deadline: Instant,
    answered: bool,
}

impl SuWatch {
    /// 密码提示识别（原始字节匹配，编码无关）：
    /// "assword" 覆盖 Password:/password:；UTF-8 与 GBK 的「密码」
    fn is_password_prompt(chunk: &[u8]) -> bool {
        const MARKERS: [&[u8]; 4] = [
            b"assword",
            "密码".as_bytes(),
            &[0xC3, 0xDC, 0xC2, 0xEB], // GBK「密码」
            "口令".as_bytes(),
        ];
        MARKERS.iter().any(|m| {
            chunk
                .windows(m.len())
                .any(|w| w.eq_ignore_ascii_case(m) || w == *m)
        })
    }
}

/// 登录宏待发状态：su 密码应答成功后一次性下发（take 后不复位）
struct MacroPending {
    lines: Vec<String>,
    input_enc: Option<Arc<Mutex<crate::encoding::InputEncoder>>>,
}

/// 宏行间延时：覆盖 su/切换目录类命令的处理时延；shell 未就绪由 PTY 输入缓冲兜底
const MACRO_LINE_DELAY: Duration = Duration::from_millis(300);

/// 宏文本 → 执行行：trim + 丢空行；不支持注释/变量（v1 纯下发语义）
fn macro_lines(raw: &str) -> Vec<String> {
    raw.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// 逐行下发登录宏：非 utf-8 会话经输入编码器转码；写失败（会话拆除）即停
async fn send_macro(
    lines: &[String],
    writer: &Arc<AnyWriter>,
    input_enc: Option<&Arc<Mutex<crate::encoding::InputEncoder>>>,
) {
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(MACRO_LINE_DELAY).await;
        }
        let mut bytes = match input_enc {
            Some(enc) => enc.lock().encode(line.as_bytes()),
            None => line.as_bytes().to_vec(),
        };
        bytes.push(b'\r');
        if writer.write(&bytes).await.is_err() {
            return;
        }
    }
}

struct TermSession {
    /// 重连时整枚替换（sessions 锁内 swap）
    writer: Arc<AnyWriter>,
    /// 来源会话档案 id（内联 spec 连接为 None）：随会话隧道的归属键
    session_id: Option<String>,
    /// 输入转码器（非 utf-8 会话）：UTF-8 → 目标编码；None = 直通零拷贝
    input_enc: Option<Arc<Mutex<crate::encoding::InputEncoder>>>,
    /// 信用状态（PR-6）：(tabId, streamEpoch) 累计 ACK；关闭随表项销毁，不等待 ACK
    credit: Arc<CreditState>,
    /// 最新终端尺寸：重连开 PTY 用（resize 命令实时更新）
    cols: AtomicU64,
    rows: AtomicU64,
    task: tauri::async_runtime::JoinHandle<()>,
}

/// 重连退避：1/2/4/8/16s 封顶 + equal jitter（防多会话断线后同步重连）
fn reconnect_backoff(attempt: u32) -> Duration {
    core_ssh::equal_jitter(Duration::from_secs(1u64 << (attempt - 1).min(4)))
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
                    "creditAvailable": s.credit.credits.available_permits(),
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
    /// su 二级登录（批次二十二）：None = 不启用；重连后自动重放
    su: Option<SuConfig>,
    /// 登录宏（已解析行）：每轮 shell（首连/重连）重放；空 = 无
    login_macro: Vec<String>,
    /// 输入转码器（宏文本非 utf-8 会话转码用；None = 直通）
    input_enc: Option<Arc<Mutex<crate::encoding::InputEncoder>>>,
    /// 当前写半（重连即换）：su 命令与密码应答的写入通道
    writer: Arc<AnyWriter>,
    /// 输出转码（非 utf-8 会话）：读循环内建 Decoder，重连即重建
    out_encoding: Option<&'static encoding_rs::Encoding>,
    data: Channel<Response>,
    events: Channel<Value>,
    credit: Arc<CreditState>,
    reader: AnyReader,
}

async fn supervise(mut ctx: SuperviseCtx) {
    loop {
        // su 二级登录：每轮新 shell（首连/重连）重放一次。先入队 su 命令（shell 未就绪
        // 时 PTY 输入缓冲会保留），read_loop 里一次性 expect 密码提示并应答。
        // 配置 command 的会话不 su——command 取代登录 shell，su 无意义
        let mut su_watch = match (&ctx.backend, &ctx.su) {
            (Backend::Ssh(rc), Some(su)) if rc.command.is_none() => {
                let cmd = format!("su - {}\r", su.user);
                let _ = ctx.writer.write(cmd.as_bytes()).await;
                Some(SuWatch {
                    password: su.password.clone(),
                    deadline: Instant::now() + Duration::from_secs(30),
                    answered: false,
                })
            }
            _ => None,
        };
        // 登录宏（重连每轮重放，与 su 语义一致）：
        // - 配 command 的会话不执行（command 取代登录 shell，无交互行可下发）；
        // - 无 su：立即 spawn 逐行下发（PTY 输入缓冲保留到 shell 就绪）；
        // - su + 保险库密码：宏传 read_loop，su 应答成功后下发；
        // - su 手输密码：应答时机不可知，跳过并告知（fail-loud 不猜）
        let mut macro_pending = None;
        if !ctx.login_macro.is_empty() {
            let commandless = match &ctx.backend {
                Backend::Ssh(rc) => rc.command.is_none(),
                Backend::Local => true,
            };
            if commandless {
                match &ctx.su {
                    None => {
                        let lines = ctx.login_macro.clone();
                        let enc = ctx.input_enc.clone();
                        let w = ctx.writer.clone();
                        tauri::async_runtime::spawn(async move {
                            send_macro(&lines, &w, enc.as_ref()).await;
                        });
                    }
                    Some(su) if su.password.is_none() => {
                        let _ = ctx.events.send(json!({
                            "v": 1, "type": "macro_skipped", "tabId": ctx.tab_id,
                            "reason": "suManualPassword",
                        }));
                    }
                    Some(_) => {
                        macro_pending = Some(MacroPending {
                            lines: ctx.login_macro.clone(),
                            input_enc: ctx.input_enc.clone(),
                        });
                    }
                }
            }
        }
        read_loop(
            &mut ctx.reader,
            &ctx.data,
            &ctx.credit,
            ctx.out_encoding,
            su_watch.as_mut(),
            &ctx.writer,
            &mut macro_pending,
        )
        .await;

        // 用户主动关闭：表项已被 term_close 摘除 → 静默退出
        if !ctx.mgr.sessions.lock().contains_key(&ctx.tab_id) {
            return;
        }
        // 意外断开（用户关闭已在上方静默返回）：掉线定位证据链——读循环为何结束
        // 由 core-ssh PtyReader 记 EOF/Close/传输终止，此处记会话级后续动作
        tracing::info!(tab_id = %ctx.tab_id, "终端读循环结束（非用户关闭），进入重连判定");

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
        // 换写半（输入路径无感 + ctx.writer 同步：su 重放用）；表项中途被摘则放弃
        let new_writer = Arc::new(AnyWriter::Ssh(writer));
        {
            let mut sessions = ctx.mgr.sessions.lock();
            match sessions.get_mut(&ctx.tab_id) {
                Some(s) => s.writer = new_writer.clone(),
                None => return,
            }
        }
        ctx.writer = new_writer;
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
            tracing::warn!(tab_id = %ctx.tab_id, attempts = max_attempts, "重连次数耗尽，会话关闭");
            return None;
        }
        let _ = ctx.events.send(json!({
            "v": 1, "type": "session_state",
            "tabId": ctx.tab_id, "state": "reconnecting", "attempt": attempt,
        }));
        tracing::info!(tab_id = %ctx.tab_id, attempt, "会话意外断开，准备重连");
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
// 参数仅内部装配，非公开 API；clippy 参数数误伤豁免
#[allow(clippy::too_many_arguments)]
async fn read_loop(
    reader: &mut AnyReader,
    data_ch: &Channel<Response>,
    credit: &Arc<CreditState>,
    out_encoding: Option<&'static encoding_rs::Encoding>,
    // su 二级登录：一次性密码 expect（Some = 本轮 shell 武装中）
    mut su_watch: Option<&mut SuWatch>,
    writer: &Arc<AnyWriter>,
    // 登录宏：su 密码应答成功后一次性下发（Some = 待发；owned——spawn 需 'static）
    macro_pending: &mut Option<MacroPending>,
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
                        // su 密码应答：武装窗口内首个密码提示自动答一次；答错不重试
                        // （防锁定），超窗/已答后 pure 旁路——零常态开销
                        if let Some(w) = su_watch.as_mut() {
                            if !w.answered && Instant::now() < w.deadline {
                                if SuWatch::is_password_prompt(&bytes) {
                                    w.answered = true;
                                    if let Some(pw) = &w.password {
                                        let mut buf = pw.as_bytes().to_vec();
                                        buf.push(b'\r');
                                        let _ = writer.write(&buf).await;
                                    }
                                    // 登录宏：su 应答成功即触发（spawn 不阻塞读循环；
                                    // 密码答错时宏会落入原用户 shell——与手输等价，已知语义）
                                    if let Some(m) = macro_pending.take() {
                                        let w2 = writer.clone();
                                        tauri::async_runtime::spawn(async move {
                                            send_macro(&m.lines, &w2, m.input_enc.as_ref()).await;
                                        });
                                    }
                                }
                            } else if Instant::now() >= w.deadline {
                                su_watch = None; // 超窗解除武装
                            }
                        }
                        match decoder.as_mut() {
                            Some(dec) => dec.decode_append(&bytes, &mut agg),
                            None => agg.extend_from_slice(&bytes),
                        }
                        if agg.len() >= AGG_CAP {
                            flush(data_ch, &mut agg, credit).await;
                            flush_at = Instant::now() + AGG_WINDOW;
                        }
                    }
                    None => {
                        flush(data_ch, &mut agg, credit).await;
                        return;
                    }
                }
            }
            _ = &mut delay => {
                if !agg.is_empty() {
                    flush(data_ch, &mut agg, credit).await;
                }
                flush_at = Instant::now() + AGG_WINDOW;
            }
        }
    }
}

async fn flush(data_ch: &Channel<Response>, agg: &mut Vec<u8>, credit: &Arc<CreditState>) {
    if agg.is_empty() {
        return;
    }
    let buf = std::mem::replace(agg, Vec::with_capacity(AGG_CAP));
    // 断代（send 已失败，前端不可达）：数据直接丢弃，不再消耗信用
    if credit.is_broken() {
        return;
    }
    // 等待前端信用——背压点；等待期间读取循环挂起。
    // permit 必须 forget，否则 drop 即归还，闸门形同虚设（实测踩中）。
    match credit.credits.acquire_many(buf.len() as u32).await {
        Ok(permit) => permit.forget(),
        Err(_) => return, // 信号量关闭（会话拆除）：丢弃残余数据
    }
    // C4 顺序：分配 frameSeq/offset → 登记 sent range → send
    let Some(frame) = credit.alloc_frame(buf) else {
        return; // offset 溢出断代（信用已耗，随代际销毁）
    };
    if data_ch.send(Response::new(frame)).is_err() {
        // send 失败：该 epoch 断代——不回滚、不复用 offset、不回补信用
        credit.mark_broken();
        tracing::warn!("终端数据帧发送失败，本 streamEpoch 断代");
    }
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

    #[test]
    fn su_password_prompt_detection() {
        // 英文提示（大小写不敏感）
        assert!(SuWatch::is_password_prompt(b"Password:"));
        assert!(SuWatch::is_password_prompt(b"[sudo] password for ops: "));
        // 中文提示：UTF-8 与 GBK
        assert!(SuWatch::is_password_prompt("密码：".as_bytes()));
        assert!(SuWatch::is_password_prompt(&[
            0xC3, 0xDC, 0xC2, 0xEB, 0xA3, 0xBA
        ])); // GBK「密码：」
        assert!(SuWatch::is_password_prompt("口令:".as_bytes()));
        // 普通输出不误判
        assert!(!SuWatch::is_password_prompt(b"[ops@web ~]$ "));
        assert!(!SuWatch::is_password_prompt(
            "普通命令输出，无任何提示词".as_bytes()
        ));
    }

    #[test]
    fn macro_lines_trim_and_skip_empty() {
        // 空/全空白 → 无宏
        assert!(macro_lines("").is_empty());
        assert!(macro_lines("  \n \n\r\n").is_empty());
        // trim + 丢空行 + 保留序
        assert_eq!(
            macro_lines("  sudo -i \ncd /data/app\n\n tail -f logs/app.log \n"),
            vec!["sudo -i", "cd /data/app", "tail -f logs/app.log"]
        );
        // 不做注释/变量解释：# 开头照发（v1 纯下发语义）
        assert_eq!(macro_lines("# not a comment"), vec!["# not a comment"]);
    }

    // ---- PR-6 累计 ACK 协议（CreditState）----

    fn decode_frame(frame: &[u8]) -> (u64, u64, u64, u64, &[u8]) {
        let g = |i: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&frame[i..i + 8]);
            u64::from_le_bytes(b)
        };
        (g(0), g(8), g(16), g(24), &frame[FRAME_HEADER_LEN..])
    }

    /// 消耗 n 字节信用（模拟 flush 前 acquire+forget）
    fn consume_credit(c: &CreditState, n: u32) {
        match c.credits.try_acquire_many(n) {
            Ok(permit) => permit.forget(),
            Err(_) => panic!("信用应充足"),
        }
    }

    /// 组帧（应成功；None 即断代/溢出，属本组测试的失败信号）
    fn must_frame(c: &CreditState, payload: Vec<u8>) -> Vec<u8> {
        match c.alloc_frame(payload) {
            Some(f) => f,
            None => panic!("alloc_frame 应成功"),
        }
    }

    #[test]
    fn credit_frame_header_layout_and_register() {
        let c = CreditState::new(7);
        let f0 = must_frame(&c, vec![1, 2, 3]);
        let (epoch, seq, start, end, payload) = decode_frame(&f0);
        assert_eq!((epoch, seq, start, end), (7, 0, 0, 3));
        assert_eq!(payload, &[1, 2, 3]);
        // 第二帧：seq/offset 接续
        let (_, seq1, start1, end1, _) = decode_frame(&must_frame(&c, vec![4]));
        assert_eq!((seq1, start1, end1), (1, 3, 4));
        // sent range 先于 send 登记（C4）
        assert_eq!(c.sent_total.load(Ordering::Relaxed), 4);
        assert_eq!(c.outstanding(), 4);
    }

    #[test]
    fn credit_ack_stale_epoch_dropped() {
        let c = CreditState::new(7);
        consume_credit(&c, 100);
        must_frame(&c, vec![0; 100]);
        // attach 换代后旧 callback 的迟到 ACK（旧 epoch）：无条件丢弃，不回补信用
        assert_eq!(c.ack(8, 100), 0);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize - 100);
        assert_eq!(c.outstanding(), 100);
    }

    #[test]
    fn credit_ack_cumulative_release() {
        let c = CreditState::new(7);
        consume_credit(&c, 300);
        must_frame(&c, vec![0; 100]);
        must_frame(&c, vec![0; 200]);
        // 累计 ACK：一次确认到 300 → 回补全部 300 permits
        assert_eq!(c.ack(7, 300), 300);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize);
        assert_eq!(c.outstanding(), 0);
        // 新 epoch（WebView 重载后从零开始）：新代际计数独立，正常放行
        let c2 = CreditState::new(8);
        consume_credit(&c2, 50);
        must_frame(&c2, vec![0; 50]);
        assert_eq!(c2.ack(8, 50), 50);
        assert_eq!(c2.credits.available_permits(), CREDIT_HIGH as usize);
    }

    #[test]
    fn credit_ack_duplicate_or_regressed_ignored() {
        let c = CreditState::new(7);
        consume_credit(&c, 300);
        must_frame(&c, vec![0; 300]);
        assert_eq!(c.ack(7, 200), 200);
        // 同 epoch 重复 ACK / 倒退 ACK：忽略
        assert_eq!(c.ack(7, 200), 0);
        assert_eq!(c.ack(7, 150), 0);
        assert_eq!(c.outstanding(), 100);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize - 100);
    }

    #[test]
    fn credit_ack_beyond_sent_clamped() {
        let c = CreditState::new(7);
        consume_credit(&c, 100);
        must_frame(&c, vec![0; 100]);
        // ACK 超过已发送量：钳制到 sent_total
        assert_eq!(c.ack(7, 1_000_000), 100);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize);
    }

    #[test]
    fn credit_send_failure_breaks_epoch() {
        let c = CreditState::new(7);
        consume_credit(&c, 100);
        must_frame(&c, vec![0; 100]);
        c.mark_broken();
        // 断代后不允许同 epoch 继续发送
        assert!(c.alloc_frame(vec![1]).is_none());
        // send 失败批次的迟到 ACK 不得回补信用（该批次未被前端消费）
        assert_eq!(c.ack(7, 100), 0);
        assert_eq!(c.credits.available_permits(), CREDIT_HIGH as usize - 100);
    }

    #[test]
    fn credit_offset_overflow_breaks_epoch() {
        let c = CreditState::new(7);
        c.sent_total.store(u64::MAX - 10, Ordering::Relaxed);
        // u64 边界：不回绕，断代
        assert!(c.alloc_frame(vec![0; 20]).is_none());
        assert!(c.is_broken());
        assert_eq!(c.ack(7, u64::MAX), 0);
    }
}
