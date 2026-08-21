//! 隧道管理器。M0 仅类型面；中继循环与池随 M2 实现。

use std::collections::HashMap;
use std::sync::Mutex;

/// 重连期间新到本地连接的策略（规格书稳定性条：不得静默丢弃，不得无限堆积）
#[derive(Debug, Clone, Copy)]
pub enum DisconnectPolicy {
    /// 排队，带容量上限
    Queue { cap: usize },
    /// 立即失败（快速失败，调用方立即可见）
    FailFast,
}

#[derive(Debug, Clone)]
pub enum TunnelKind {
    /// 本地 -L：bind 为 (host, port)
    Local { bind: (String, u16) },
    /// 远程 -R
    Remote { bind: (String, u16) },
    /// 动态 SOCKS5
    DynamicSocks5 { bind: (String, u16) },
}

#[derive(Debug, Clone)]
pub struct TunnelSpec {
    pub session_id: String,
    pub kind: TunnelKind,
    /// Local 必填：(host, port)
    pub target: Option<(String, u16)>,
    pub auto_start: bool,
    pub max_conns: usize,
    pub on_disconnect: DisconnectPolicy,
}

#[derive(Debug, Clone, Copy)]
pub enum TunnelStatus {
    Starting,
    Listening,
    Reconnecting { attempt: u32 },
    Stopped,
    Failed,
}

/// 1Hz 聚合快照；数据路径只写原子计数器（规格书第 9 条）
#[derive(Debug, Clone, Copy, Default)]
pub struct TunnelStats {
    pub active_conns: u64,
    pub total_conns: u64,
    pub bytes_up: u64,
    pub bytes_down: u64,
    pub rate_up: u64,
    pub rate_down: u64,
    pub pool_size: u8,
    pub reconnects: u32,
}

#[derive(Default)]
pub struct TunnelManager {
    tunnels: Mutex<HashMap<String, TunnelStatus>>,
}

impl TunnelManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self) -> Vec<(String, TunnelStatus)> {
        let Ok(g) = self.tunnels.lock() else {
            return Vec::new();
        };
        g.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }
}
