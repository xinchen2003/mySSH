//! 快照类型：一轮采集的对外视图。serde camelCase 直供 IPC。

use serde::Serialize;

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    /// 采集完成的本地时间（epoch ms）。
    #[ts(type = "number")]
    pub ts_ms: u64,
    /// 与上一轮的实测间隔（首轮 0）。
    #[ts(type = "number")]
    pub interval_ms: u64,
    /// CPU 忙率 0..100（首轮 None，需两轮差分）。
    pub cpu_busy_pct: Option<f32>,
    pub load: [f32; 3],
    pub procs_running: u32,
    pub procs_total: u32,
    #[ts(type = "number")]
    pub mem_total_kb: u64,
    #[ts(type = "number")]
    pub mem_avail_kb: u64,
    #[ts(type = "number")]
    pub swap_total_kb: u64,
    #[ts(type = "number")]
    pub swap_free_kb: u64,
    #[ts(inline)]
    pub disks: Vec<DiskRate>,
    #[ts(inline)]
    pub nets: Vec<NetRate>,
    /// CPU 占用 Top（ps 口径为进程存活期平均）。
    #[ts(inline)]
    pub procs: Vec<ProcInfo>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct DiskRate {
    pub name: String,
    /// 首轮 None；此后为 B/s（sector=512B 差分换算）。
    #[ts(type = "number | null")]
    pub read_bps: Option<u64>,
    #[ts(type = "number | null")]
    pub write_bps: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct NetRate {
    pub iface: String,
    #[ts(type = "number | null")]
    pub rx_bps: Option<u64>,
    #[ts(type = "number | null")]
    pub tx_bps: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct ProcInfo {
    pub pid: u32,
    #[ts(type = "number")]
    pub rss_kb: u64,
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub comm: String,
}
