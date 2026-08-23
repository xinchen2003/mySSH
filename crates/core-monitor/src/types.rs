//! 快照类型：一轮采集的对外视图。serde camelCase 直供 IPC。

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetricsSnapshot {
    /// 采集完成的本地时间（epoch ms）。
    pub ts_ms: u64,
    /// 与上一轮的实测间隔（首轮 0）。
    pub interval_ms: u64,
    /// CPU 忙率 0..100（首轮 None，需两轮差分）。
    pub cpu_busy_pct: Option<f32>,
    pub load: [f32; 3],
    pub procs_running: u32,
    pub procs_total: u32,
    pub mem_total_kb: u64,
    pub mem_avail_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
    pub disks: Vec<DiskRate>,
    pub nets: Vec<NetRate>,
    /// CPU 占用 Top（ps 口径为进程存活期平均）。
    pub procs: Vec<ProcInfo>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskRate {
    pub name: String,
    /// 首轮 None；此后为 B/s（sector=512B 差分换算）。
    pub read_bps: Option<u64>,
    pub write_bps: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NetRate {
    pub iface: String,
    pub rx_bps: Option<u64>,
    pub tx_bps: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcInfo {
    pub pid: u32,
    pub rss_kb: u64,
    pub cpu_pct: f32,
    pub mem_pct: f32,
    pub comm: String,
}
