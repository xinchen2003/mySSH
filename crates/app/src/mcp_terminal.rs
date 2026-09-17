//! MCP 交互式终端工具：terminal_open / terminal_send / terminal_read / terminal_close。
//!
//! 与 ssh_exec 的一次性 exec 不同，本模块持有有状态 PTY shell——agent 可 cd、
//! 进 mysql、激活 venv，状态跨调用保持。terminal_open 建独立 Bulk 连接并开
//! xterm PTY（120x32），读任务单消费者循环把输出追加进 1MiB 环形缓冲
//!（超界丢最旧并置 truncated 标志）；terminal_read 以 offset 全局字节游标
//! 轮询（wait_seconds 服务端长轮询，上限 600s）；terminal_close 关通道并
//! abort 读任务。MCP 服务 stop/重启时 close_all 清场，不留孤儿连接。
//! 建连安全策略与 ssh_exec 一致：host key fail-closed（未知/变更指纹拒绝，
//! 需先在 UI 首连确认）、keyboard-interactive 无应答通路预拒；open/close
//! 落审计（actor=mcp，动作 mcp_terminal_open/mcp_terminal_close）。

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use core_ssh::{
    ConnClass, ConnectOptions, HostKeyCheck, HostKeyDecision, HostKeyPrompt, KeepaliveConfig,
    KnownHostsPolicy, PtyChannel, PtyWriter, SshConnection,
};
use core_store::Store;

/// 终端输出环形缓冲容量：超界丢最旧（输出要进模型上下文，必须有界）
const TERM_BUF_CAP: usize = 1024 * 1024;
/// terminal_read 服务端长轮询间隔（仿 wait_transfer）
const READ_POLL: Duration = Duration::from_millis(500);
/// terminal_read 单次 wait 上限（秒；与 sftp_upload/download 的 wait_seconds 一致）
const READ_WAIT_MAX: u64 = 600;
/// terminal_open 建连 + 开 PTY 的超时（对齐 ssh_exec 默认预算）
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);

/// 终端输出环形缓冲。offset 为全局字节游标：缓冲起点 = total - buf.len()。
#[derive(Default)]
struct TermBuf {
    buf: VecDeque<u8>,
    /// 累计写入字节数（只增）
    total: u64,
    /// 曾因容量超界丢弃过最旧数据（旧 offset 可能已失效）
    truncated: bool,
    /// 读任务 EOF/Close 或 terminal_close 后置位
    closed: bool,
}

impl TermBuf {
    /// 追加输出；超容量时丢弃最旧字节并置 truncated 标志
    fn append(&mut self, data: &[u8]) {
        self.total += data.len() as u64;
        self.buf.extend(data);
        if self.buf.len() > TERM_BUF_CAP {
            let drop = self.buf.len() - TERM_BUF_CAP;
            self.buf.drain(..drop);
            self.truncated = true;
        }
    }

    /// 缓冲中最早可用字节的全局 offset
    fn start(&self) -> u64 {
        self.total - self.buf.len() as u64
    }
}

/// 单个交互式终端会话（terminal_open 创建，注册进 McpTerminalState）
struct McpTermSession {
    /// 会话档案 id（terminal_close 审计用）
    session_id: String,
    writer: PtyWriter,
    /// 与读任务共享的输出缓冲
    shared: Arc<Mutex<TermBuf>>,
    /// 读任务句柄（close 时 abort）
    task: tauri::async_runtime::JoinHandle<()>,
    /// 保持连接存活：SshConnection drop 即断连，会带走 PTY 通道
    conn: SshConnection,
}

/// MCP 终端注册表：terminalId → 会话
#[derive(Default)]
pub struct McpTerminalState {
    terms: Mutex<HashMap<String, Arc<McpTermSession>>>,
}

impl McpTerminalState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 清场：MCP 服务停止/重启时关闭全部终端（关通道 + abort 读任务）
    pub async fn close_all(&self) {
        let all: Vec<Arc<McpTermSession>> = self.terms.lock().drain().map(|(_, s)| s).collect();
        for sess in all {
            if !sess.conn.is_closed() {
                let _ = sess.writer.close().await;
            }
            sess.task.abort();
            sess.shared.lock().closed = true;
        }
    }
}

/// terminal_* 工具分发（外层 call_tool 已按 terminal_ 前缀过滤，此处校验具体名）
pub(crate) async fn terminal_tool(
    store: &Arc<Store>,
    terms: &Arc<McpTerminalState>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    match name {
        "terminal_open" => terminal_open(store, terms, args).await,
        "terminal_send" => terminal_send(terms, args).await,
        "terminal_read" => terminal_read(terms, args).await,
        "terminal_close" => terminal_close(store, terms, args).await,
        other => Err(format!("未知工具：{other}")),
    }
}

/// terminal_open：resolve → KI 预拒 → Bulk 独立连接（host key fail-closed）→
/// xterm PTY → 注册 + spawn 读任务 → 返回 terminalId
async fn terminal_open(
    store: &Arc<Store>,
    terms: &Arc<McpTerminalState>,
    args: &Value,
) -> Result<String, String> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "terminal_open 缺少参数 session_id".to_string())?;
    let target = crate::sessions::resolve_session_target(store, session_id).await?;
    let spec = match target {
        crate::sessions::ResolvedTarget::Ssh(s) => s,
        crate::sessions::ResolvedTarget::Local(_) => {
            return Err("本地会话不支持 terminal_open（仅 SSH 会话）".into());
        }
    };
    // KI 无应答通路：预拒（照 sftp.rs ensure_ctx）
    if matches!(spec.auth, crate::terminal::AuthSpec::KeyboardInteractive)
        || spec
            .jump_chain
            .iter()
            .any(|h| matches!(h.auth, crate::terminal::AuthSpec::KeyboardInteractive))
    {
        return Err(
            "keyboard-interactive 不适用于 MCP 终端（无应答通路，请改用密钥/agent）".into(),
        );
    }
    let host = spec.host.clone();
    let run = terminal_open_inner(spec);
    let (conn, pty) = match tokio::time::timeout(OPEN_TIMEOUT, run).await {
        Ok(r) => r?,
        Err(_) => {
            return Err(format!(
                "终端打开超时（{}s），连接已断开",
                OPEN_TIMEOUT.as_secs()
            ));
        }
    };

    let (mut reader, writer) = pty.split();
    let id = gen_term_id();
    let shared = Arc::new(Mutex::new(TermBuf::default()));
    // 读任务：PTY 读半单消费者循环追加缓冲；EOF/Close → 置 closed 退出
    let task = {
        let shared = Arc::clone(&shared);
        tauri::async_runtime::spawn(async move {
            while let Some(data) = reader.next_data().await {
                shared.lock().append(&data);
            }
            shared.lock().closed = true;
        })
    };
    let sess = Arc::new(McpTermSession {
        session_id: session_id.to_string(),
        writer,
        shared,
        task,
        conn,
    });
    terms.terms.lock().insert(id.clone(), sess);
    audit(
        store,
        session_id,
        "mcp_terminal_open",
        &json!({ "terminalId": id, "host": host }),
    )
    .await;
    tracing::info!(session_id, terminal_id = %id, host = %host, "MCP 终端已打开");
    Ok(json!({ "terminalId": id }).to_string())
}

/// 建连 + 开 PTY：host key fail-closed（照 ssh_exec_inner；无 UI 弹窗通路）
async fn terminal_open_inner(
    spec: crate::terminal::TermOpenSpec,
) -> Result<(SshConnection, PtyChannel), String> {
    let opts = ConnectOptions {
        host: spec.host.clone(),
        port: spec.port,
        user: spec.user.clone(),
        auth: crate::terminal::auth_method_from(&spec.auth),
        jump_chain: crate::terminal::jump_chain_from(&spec.jump_chain),
        // Bulk 语义：不占交互连接，与 ssh_exec/SFTP 一致
        class: ConnClass::Bulk,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        // 已知主机直过；未知/变更 fail-closed 拒绝，提示先在 UI 首连确认指纹
        host_key_check: HostKeyCheck::KnownHosts(KnownHostsPolicy {
            path: crate::terminal::known_hosts_path(),
            prompter: Arc::new(|_: HostKeyPrompt| async { HostKeyDecision::Reject }),
        }),
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
    let pty = conn
        .open_pty("xterm", 120, 32, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok((conn, pty))
}

/// terminal_send：写 UTF-8 文本；append_newline=true 时末尾补 \n
///（缺省 false，换行由 agent 自行包含在 data 中）
async fn terminal_send(terms: &Arc<McpTerminalState>, args: &Value) -> Result<String, String> {
    let id = req_term_id(args, "terminal_send")?;
    let data = args
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| "terminal_send 缺少参数 data".to_string())?;
    let append_newline = args
        .get("append_newline")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let sess = terms
        .terms
        .lock()
        .get(id)
        .cloned()
        .ok_or_else(|| format!("终端 {id} 不存在或已关闭"))?;
    if sess.shared.lock().closed {
        return Err(format!("终端 {id} 已关闭（远端 shell 已退出或连接断开）"));
    }
    let mut bytes = data.as_bytes().to_vec();
    if append_newline {
        bytes.push(b'\n');
    }
    sess.writer.write(&bytes).await.map_err(|e| e.to_string())?;
    Ok(json!({ "ok": true }).to_string())
}

/// terminal_read：offset 游标轮询。wait_seconds>0 时服务端长轮询到有新输出/
/// 终端关闭/等待到期（上限 600s），不往模型上下文塞空轮询。旧 offset 因
/// 环形丢弃失效时 truncated=true 并从最早可用处返回。
async fn terminal_read(terms: &Arc<McpTerminalState>, args: &Value) -> Result<String, String> {
    let id = req_term_id(args, "terminal_read")?;
    let offset = args
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| "terminal_read 缺少参数 offset".to_string())?;
    let wait = args
        .get("wait_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(READ_WAIT_MAX);
    let sess = terms
        .terms
        .lock()
        .get(id)
        .cloned()
        .ok_or_else(|| format!("终端 {id} 不存在或已关闭"))?;
    let deadline = Instant::now() + Duration::from_secs(wait);
    loop {
        {
            let b = sess.shared.lock();
            // 有新输出（含 offset 已失效）、终端关闭或等待到期 → 返回当前快照
            if offset < b.total || b.closed || Instant::now() >= deadline {
                let start = b.start();
                let from = offset.max(start);
                let skip = (from - start) as usize;
                let data: Vec<u8> = b.buf.iter().skip(skip).copied().collect();
                return Ok(json!({
                    "data": String::from_utf8_lossy(&data),
                    "nextOffset": from + data.len() as u64,
                    "closed": b.closed,
                    "truncated": b.truncated && offset < start,
                })
                .to_string());
            }
        }
        tokio::time::sleep(READ_POLL).await;
    }
}

/// terminal_close：移出注册表 → 关通道 → abort 读任务（落审计）
async fn terminal_close(
    store: &Arc<Store>,
    terms: &Arc<McpTerminalState>,
    args: &Value,
) -> Result<String, String> {
    let id = req_term_id(args, "terminal_close")?;
    let sess = terms
        .terms
        .lock()
        .remove(id)
        .ok_or_else(|| format!("终端 {id} 不存在或已关闭"))?;
    if !sess.conn.is_closed() {
        let _ = sess.writer.close().await;
    }
    sess.task.abort();
    sess.shared.lock().closed = true;
    audit(
        store,
        &sess.session_id,
        "mcp_terminal_close",
        &json!({ "terminalId": id }),
    )
    .await;
    tracing::info!(terminal_id = %id, "MCP 终端已关闭");
    Ok(json!({ "closed": true }).to_string())
}

/// 生成随机 terminal id（16 字节 hex，仿 mcp.rs gen_token）
fn gen_term_id() -> String {
    let bytes: [u8; 16] = rand::random();
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn req_term_id<'a>(args: &'a Value, tool: &str) -> Result<&'a str, String> {
    args.get("terminalId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{tool} 缺少参数 terminalId"))
}

/// MCP 侧写操作审计（Actor::Mcp，照 mcp.rs mcp_audit）
async fn audit(store: &Arc<Store>, session_id: &str, action: &str, detail: &Value) {
    let _ = store
        .audit()
        .append(core_store::Actor::Mcp, Some(session_id), action, detail)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn term_buf_ring_drops_oldest() {
        let mut b = TermBuf::default();
        b.append(&vec![1u8; TERM_BUF_CAP]);
        assert_eq!(b.start(), 0);
        assert!(!b.truncated);
        // 再写 10 字节 → 丢最旧 10 字节，起点前移
        b.append(&[2u8; 10]);
        assert!(b.truncated);
        assert_eq!(b.start(), 10);
        assert_eq!(b.total, TERM_BUF_CAP as u64 + 10);
        assert_eq!(b.buf.len(), TERM_BUF_CAP);
        assert_eq!(b.buf[0], 1);
        assert_eq!(b.buf[TERM_BUF_CAP - 1], 2);
    }

    #[test]
    fn term_buf_oversized_chunk_keeps_tail() {
        let mut b = TermBuf::default();
        // 单块超容量：只保留尾部 CAP 字节
        let mut data = vec![0u8; TERM_BUF_CAP + 5];
        data[TERM_BUF_CAP + 4] = 7;
        b.append(&data);
        assert!(b.truncated);
        assert_eq!(b.start(), 5);
        assert_eq!(b.buf.len(), TERM_BUF_CAP);
        assert_eq!(b.buf[TERM_BUF_CAP - 1], 7);
    }
}
