//! 采集器：每轮一个 exec channel 跑组合脚本，差分出速率。

use std::collections::HashMap;
use std::time::Instant;

use core_ssh::SshConnection;
use tracing::debug;

use crate::error::MonitorError;
use crate::parser::{self, RawSample};
use crate::types::{DiskRate, MetricsSnapshot, NetRate};

/// 单轮采集脚本：段头用 `==NAME==`，逐项 2>/dev/null 容错（BusyBox ps 无 -o 时进程表为空，其余不受影响）。
const COLLECT_SCRIPT: &str = concat!(
    "echo ==STAT==; cat /proc/stat 2>/dev/null; ",
    "echo ==MEM==; cat /proc/meminfo 2>/dev/null; ",
    "echo ==LOAD==; cat /proc/loadavg 2>/dev/null; ",
    "echo ==DISK==; cat /proc/diskstats 2>/dev/null; ",
    "echo ==NET==; cat /proc/net/dev 2>/dev/null; ",
    "echo ==PS==; ps -eo pid=,rss=,pcpu=,pmem=,comm= --sort=-pcpu 2>/dev/null | head -n 16"
);

const SECTOR_BYTES: u64 = 512;

/// 持有上一轮原始采样做差分；每会话一个实例。
pub struct MetricsCollector {
    prev: Option<(RawSample, Instant)>,
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsCollector {
    pub fn new() -> Self {
        Self { prev: None }
    }

    /// 采集一轮。`conn` 应为 Bulk 类连接（与 SFTP/隧道同池，不占交互连接）。
    /// 目标机无 /proc（非 Linux）→ `NoProcfs`；其余字段缺失静默降级。
    pub async fn collect(&mut self, conn: &SshConnection) -> Result<MetricsSnapshot, MonitorError> {
        let out = conn.exec_collect(COLLECT_SCRIPT).await?;
        let text = String::from_utf8_lossy(&out);
        let raw = parser::parse_round(&text);
        // 核心字段：meminfo 或 loadavg 都没有 → 判定非 /proc 系统
        if raw.mem.is_none() && raw.load.is_none() {
            debug!(
                "monitor: no procfs signature in output ({} bytes)",
                out.len()
            );
            return Err(MonitorError::NoProcfs);
        }
        let now = Instant::now();
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let prev = self.prev.take();
        let dt_secs = prev
            .as_ref()
            .map(|(_, t)| now.duration_since(*t).as_secs_f32())
            .filter(|d| *d > 0.05);
        let interval_ms = dt_secs.map(|d| (d * 1000.0) as u64).unwrap_or(0);

        let cpu_busy_pct = match (prev.as_ref().and_then(|(p, _)| p.cpu), raw.cpu) {
            (Some(a), Some(b)) if b.total() > a.total() => {
                let dt = (b.total() - a.total()) as f32;
                Some(100.0 * (1.0 - (b.idle_all() - a.idle_all()) as f32 / dt))
            }
            _ => None,
        };

        let prev_disks: HashMap<&str, (u64, u64)> = prev
            .as_ref()
            .map(|(p, _)| {
                p.disks
                    .iter()
                    .map(|(n, r, w)| (n.as_str(), (*r, *w)))
                    .collect()
            })
            .unwrap_or_default();
        let disks = raw
            .disks
            .iter()
            .map(|(n, r, w)| {
                let rates = prev_disks
                    .get(n.as_str())
                    .zip(dt_secs)
                    .map(|((pr, pw), dt)| {
                        (
                            rate(r.saturating_sub(*pr) * SECTOR_BYTES, dt),
                            rate(w.saturating_sub(*pw) * SECTOR_BYTES, dt),
                        )
                    });
                DiskRate {
                    name: n.clone(),
                    read_bps: rates.map(|(r, _)| r),
                    write_bps: rates.map(|(_, w)| w),
                }
            })
            .collect();

        let prev_nets: HashMap<&str, (u64, u64)> = prev
            .as_ref()
            .map(|(p, _)| {
                p.nets
                    .iter()
                    .map(|(n, rx, tx)| (n.as_str(), (*rx, *tx)))
                    .collect()
            })
            .unwrap_or_default();
        let nets = raw
            .nets
            .iter()
            .map(|(n, rx, tx)| {
                let rates = prev_nets
                    .get(n.as_str())
                    .zip(dt_secs)
                    .map(|((prx, ptx), dt)| {
                        (
                            rate(rx.saturating_sub(*prx), dt),
                            rate(tx.saturating_sub(*ptx), dt),
                        )
                    });
                NetRate {
                    iface: n.clone(),
                    rx_bps: rates.map(|(r, _)| r),
                    tx_bps: rates.map(|(_, t)| t),
                }
            })
            .collect();

        let load = raw.load.unwrap_or(crate::parser::LoadAvg {
            l1: 0.0,
            l5: 0.0,
            l15: 0.0,
            running: 0,
            total: 0,
        });
        let mem = raw.mem.unwrap_or(crate::parser::MemKb {
            total: 0,
            avail: 0,
            swap_total: 0,
            swap_free: 0,
        });

        let procs = raw.procs.clone();
        self.prev = Some((raw, now));
        Ok(MetricsSnapshot {
            ts_ms,
            interval_ms,
            cpu_busy_pct,
            load: [load.l1, load.l5, load.l15],
            procs_running: load.running,
            procs_total: load.total,
            mem_total_kb: mem.total,
            mem_avail_kb: mem.avail,
            swap_total_kb: mem.swap_total,
            swap_free_kb: mem.swap_free,
            disks,
            nets,
            procs,
        })
    }
}

fn rate(delta_bytes: u64, dt_secs: f32) -> u64 {
    (delta_bytes as f64 / dt_secs as f64) as u64
}
