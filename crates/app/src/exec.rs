//! Exec Transport 连接组（性能优化 PR-14）：每会话一条后台 SSH 连接承载
//! Monitor 与 MCP ssh_exec 的 exec channel，与 SFTP/Tunnel Transport 并列
//! （后台 Transport 预算：每 Session ≤3）。
//!
//! - 开监控不再创建 SFTP ctx（旧路径 ensure_ctx 连带建双 subsystem）。
//! - 各自独立 channel semaphore 配额：Monitor 保留最低配额（防 MCP 突发
//!   exec 饿死监控采样），MCP 并发上限。
//! - 复用 SubsystemSlot 单飞状态机：并发 ensure 只握手一次、失败退避、
//!   Transport 死亡/invalidate 后新代际重建。
//! - KI 闭环定义：后台 Exec Transport 无 UI 弹窗通路——KI-only 会话一律
//!   明确报错引导改用密钥/agent（无 UI 交互时不尝试 KI；并发 ensure 单飞，
//!   不存在重复弹窗问题）。
//! - TTL：零使用闲置 ≥10min 摘除（与 PR-13 ctx 回收同参数；无 subsystem 层，
//!   单层 reclaim）。监控订阅期间每次采样 touch，自然不会误收。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use core_sftp::SubsystemSlot;

use crate::sessions::resolve_session_spec;

/// Monitor exec channel 最低保留配额
const MONITOR_PERMITS: usize = 2;
/// MCP ssh_exec 并发上限
const MCP_PERMITS: usize = 4;
/// MCP 取 permit 最长等待（超时明确拒绝，不无限排队）
const MCP_PERMIT_WAIT_MS: u64 = 3000;
/// 闲置回收阈值（与 PR-13 ctx TTL 一致）
const EXEC_IDLE_TTL_MS: u64 = 10 * 60 * 1000;
const SWEEP_INTERVAL_SECS: u64 = 60;

/// 每会话的 Exec 上下文（一条 Transport + 两组 channel 配额）
pub(crate) struct ExecCtx {
    conn: Arc<core_ssh::SshConnection>,
    monitor_permits: Arc<tokio::sync::Semaphore>,
    mcp_permits: Arc<tokio::sync::Semaphore>,
    last_used: AtomicU64,
}

impl ExecCtx {
    pub(crate) fn conn(&self) -> &core_ssh::SshConnection {
        &self.conn
    }

    /// exec channel 开道用句柄（Arc 克隆；调用方 drop 不影响组内其他使用者）
    pub(crate) fn conn_handle(&self) -> Arc<core_ssh::SshConnection> {
        self.conn.clone()
    }

    fn touch(&self) {
        self.last_used.store(now_millis(), Ordering::Relaxed);
    }

    fn idle_ms(&self) -> u64 {
        now_millis().saturating_sub(self.last_used.load(Ordering::Relaxed))
    }

    /// Monitor 采样配额（保留最低额度；等待采集周期内必得，不设超时）
    pub(crate) async fn acquire_monitor(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.touch();
        self.monitor_permits.clone().acquire_owned().await.ok()
    }

    /// MCP exec 配额（并发上限 + 3s 等待超时明确拒绝）
    pub(crate) async fn acquire_mcp(&self) -> Result<tokio::sync::OwnedSemaphorePermit, String> {
        self.touch();
        match tokio::time::timeout(
            std::time::Duration::from_millis(MCP_PERMIT_WAIT_MS),
            self.mcp_permits.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(p)) => Ok(p),
            Ok(Err(_)) => Err("Exec Transport 已关闭".into()),
            Err(_) => Err(format!(
                "MCP exec 并发上限（{MCP_PERMITS}），等待 {MCP_PERMIT_WAIT_MS}ms 未获配额"
            )),
        }
    }
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

type ExecSlot = SubsystemSlot<ExecCtx>;

pub struct ExecManagerState {
    ctxs: Mutex<HashMap<String, Arc<ExecSlot>>>,
    rt: tokio::runtime::Handle,
}

impl ExecManagerState {
    /// 与 SFTP 共用 bulk-rt（设计 06：批量 IO 统一 runtime）
    pub fn new(rt: tokio::runtime::Handle) -> Arc<Self> {
        let state = Arc::new(Self {
            ctxs: Mutex::new(HashMap::new()),
            rt,
        });
        state.rt.clone().spawn(sweep_loop(state.clone()));
        state
    }

    pub(crate) fn rt(&self) -> tokio::runtime::Handle {
        self.rt.clone()
    }

    /// 会话配置变更/删除时摘除（与 SFTP ctx 同策略）
    pub(crate) fn drop_ctx(&self, session_id: &str) {
        self.ctxs.lock().remove(session_id);
    }
}

/// 取/建会话的 Exec 上下文（单飞；KI 明确拒绝；失败退避 2s 由槽提供）
pub(crate) async fn ensure_exec_ctx(
    state: &Arc<ExecManagerState>,
    store: &Arc<core_store::Store>,
    session_id: &str,
) -> Result<Arc<ExecCtx>, String> {
    let slot = {
        let mut map = state.ctxs.lock();
        match map.get(session_id) {
            Some(s) => s.clone(),
            None => {
                let s = new_exec_slot(state, store, session_id);
                map.insert(session_id.to_string(), s.clone());
                s
            }
        }
    };
    if let Some(ctx) = slot.ready() {
        if ctx.conn().is_closed() {
            slot.invalidate();
        }
    }
    let ctx = slot.get().await.map_err(|e| e.to_string())?;
    ctx.touch();
    Ok(ctx)
}

fn new_exec_slot(
    state: &Arc<ExecManagerState>,
    store: &Arc<core_store::Store>,
    session_id: &str,
) -> Arc<ExecSlot> {
    let rt = state.rt.clone();
    let store_f = store.clone();
    let sid = session_id.to_string();
    let slot = SubsystemSlot::new(
        "exec-ctx",
        rt,
        Arc::new(move || {
            let store = store_f.clone();
            let sid = sid.clone();
            Box::pin(async move {
                build_exec_ctx(&store, &sid)
                    .await
                    .map_err(core_sftp::SftpError::Subsystem)
            })
        }),
        Arc::new(|ctx| {
            Box::pin(async move {
                if ctx.conn().is_closed() {
                    Err(core_sftp::SftpError::Subsystem("transport closed".into()))
                } else {
                    Ok(())
                }
            })
        }),
    );
    slot.set_retry_backoff(std::time::Duration::from_secs(2));
    slot
}

async fn build_exec_ctx(
    store: &Arc<core_store::Store>,
    session_id: &str,
) -> Result<ExecCtx, String> {
    let spec = resolve_session_spec(store, session_id).await?;
    // 后台建连策略收口于 connect 模块（卡 5）：KI 拒绝/hostkey 严格/Bulk；控制流量 4MB 档
    let conn = crate::connect::background(&spec, crate::connect::ConnectProfile::Control)
        .await
        .map_err(|e| format!("Exec 后台连接失败: {e}"))?;
    Ok(ExecCtx {
        conn: Arc::new(conn),
        monitor_permits: Arc::new(tokio::sync::Semaphore::new(MONITOR_PERMITS)),
        mcp_permits: Arc::new(tokio::sync::Semaphore::new(MCP_PERMITS)),
        last_used: AtomicU64::new(now_millis()),
    })
}

/// TTL sweep（单层 reclaim；先摘除再锁外 drop，同 PR-13 C9 约束）
async fn sweep_loop(state: Arc<ExecManagerState>) {
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(SWEEP_INTERVAL_SECS)).await;
        let mut reclaimed: Vec<Arc<ExecSlot>> = Vec::new();
        {
            let mut map = state.ctxs.lock();
            let mut victims: Vec<String> = Vec::new();
            for (sid, slot) in map.iter() {
                let Some(ctx) = slot.ready() else {
                    continue;
                };
                if ctx.idle_ms() >= EXEC_IDLE_TTL_MS {
                    victims.push(sid.clone());
                }
            }
            for sid in &victims {
                if let Some(s) = map.remove(sid) {
                    reclaimed.push(s);
                }
            }
        }
        drop(reclaimed); // 锁外关闭 Transport
    }
}
