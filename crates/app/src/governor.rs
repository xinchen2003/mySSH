//! 全局 Governor（PR-17 一期）：资源表单一事实源 + perf_json 指标出口。
//!
//! 一期范围（docs/governor-子设计.md §7）：账本（core_policy::Budget）+
//! 隧道 channel / SFTP 执行槽两类资源接入 + 指标。终端 outstanding 只出指标
//! （灰度），ring buffer 与降权属二期。

use serde_json::{json, Value};

use crate::sftp::SftpManagerState;
use crate::tunnels::TunnelManagerState;

/// 资源表条目（docs/governor-子设计.md §2；上限值引用 core_policy::budget::caps）
struct ResourceRow {
    key: &'static str,
    cap: usize,
    hard: bool,
    policy: &'static str,
}

/// 资源表：没有此表不实施（评审锚点）；cap 与构造处同源（caps::*）
const RESOURCE_TABLE: &[ResourceRow] = &[
    ResourceRow {
        key: "transport.perSession",
        cap: 3, // SFTP 1 + Exec 1 + Tunnel 组 1（C10 现状；软上限=复用）
        hard: false,
        policy: "复用/等待",
    },
    ResourceRow {
        key: "tunnel.chan",
        cap: core_policy::budget::caps::TUNNEL_CHANNEL,
        hard: true,
        policy: "等待≤3s后拒绝",
    },
    ResourceRow {
        key: "sftp.ready",
        cap: core_policy::budget::caps::SFTP_READY,
        hard: true,
        policy: "Scanner await（mpsc 有界通道背压）",
    },
    ResourceRow {
        key: "sftp.exec",
        cap: core_policy::budget::caps::SFTP_EXEC,
        hard: true,
        policy: "排队（无限等待）",
    },
    ResourceRow {
        key: "term.outstanding",
        cap: 0, // 待基准（PR-16 数据）；一期仅出指标不启用硬策略（灰度）
        hard: true,
        policy: "一期仅指标；二期降权→ring buffer 截断（C11）",
    },
    ResourceRow {
        key: "history.write",
        cap: 500, // 每 200ms 批量（PR-8 现状）
        hard: true,
        policy: "批量+背压（满即弃 file 明细，job 计数仍准）",
    },
];

/// governor 节：资源表 + 活跃账本快照
pub(crate) fn perf_json(tunnels: &TunnelManagerState, sftp: &SftpManagerState) -> Value {
    let table: Vec<Value> = RESOURCE_TABLE
        .iter()
        .map(|r| {
            json!({
                "key": r.key,
                "cap": r.cap,
                "hard": r.hard,
                "policy": r.policy,
            })
        })
        .collect();
    json!({
        "table": table,
        // 隧道 channel 预算：每组一行（active/rejected）
        "tunnelChan": tunnels.mgr.channel_budgets(),
        // SFTP 执行槽：每会话一行（含 0 活跃 ctx 也在列，便于对比）
        "sftpExec": sftp.exec_budgets(),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn resource_table_covers_six_rows() {
        // 资源表完整性锚点：设计文档 §2 的六类资源一行不少
        assert_eq!(super::RESOURCE_TABLE.len(), 6);
        assert!(super::RESOURCE_TABLE
            .iter()
            .all(|r| r.cap > 0 || r.key == "term.outstanding"));
    }
}
