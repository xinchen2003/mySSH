//! 共享测量状态：原子计数器（数据路径无锁）+ 采样序列。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

// ---- 隧道计数器（中继热路径只用 Relaxed 原子，不打日志不加锁）
pub static UP_BYTES: AtomicU64 = AtomicU64::new(0);
pub static DOWN_BYTES: AtomicU64 = AtomicU64::new(0);
pub static ACTIVE_CONNS: AtomicU64 = AtomicU64::new(0);
pub static TOTAL_CONNS: AtomicU64 = AtomicU64::new(0);
pub static CONNECT_ERRORS: AtomicU64 = AtomicU64::new(0);

/// direct-tcpip channel 建立延迟（微秒）
pub static CONNECT_LAT_US: Mutex<Vec<u64>> = Mutex::new(Vec::new());

pub fn now_wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn record_connect_lat(us: u64) {
    if let Ok(mut v) = CONNECT_LAT_US.lock() {
        v.push(us);
    }
}

#[derive(Serialize, Clone)]
pub struct ProcSample {
    pub t_wall: u64,
    /// 主进程 CPU（%，单核=100 基准，可超 100）
    pub self_cpu: f32,
    pub self_rss: u64,
    /// 关联 WebView2 进程合计
    pub wv_cpu: f32,
    pub wv_rss: u64,
}

pub static PROC_SAMPLES: Mutex<Vec<ProcSample>> = Mutex::new(Vec::new());

/// 500ms 采样主进程 + 关联 WebView2 子进程（父链在 app 下的 msedgewebview2）
pub fn start_sampler() {
    tauri::async_runtime::spawn(async {
        use sysinfo::{Pid, ProcessesToUpdate, System};
        let mut sys = System::new();
        let self_pid = Pid::from_u32(std::process::id());
        // 首次刷新建立 CPU 基准
        sys.refresh_processes(ProcessesToUpdate::All, true);
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            sys.refresh_processes(ProcessesToUpdate::All, true);

            // 父链 BFS：找出所有子孙进程中的 msedgewebview2
            let mut frontier = vec![self_pid];
            let mut wv_cpu = 0f32;
            let mut wv_rss = 0u64;
            while !frontier.is_empty() {
                let mut next = Vec::new();
                for (pid, proc_) in sys.processes() {
                    if let Some(parent) = proc_.parent() {
                        if frontier.contains(&parent) {
                            next.push(*pid);
                            if proc_.name().to_string_lossy().contains("msedgewebview2") {
                                wv_cpu += proc_.cpu_usage();
                                wv_rss += proc_.memory();
                            }
                        }
                    }
                }
                frontier = next;
            }

            let (self_cpu, self_rss) = sys
                .process(self_pid)
                .map(|p| (p.cpu_usage(), p.memory()))
                .unwrap_or((0.0, 0));
            if let Ok(mut v) = PROC_SAMPLES.lock() {
                v.push(ProcSample {
                    t_wall: now_wall_ms(),
                    self_cpu,
                    self_rss,
                    wv_cpu,
                    wv_rss,
                });
            }
        }
    });
}

#[derive(Serialize, Clone, Copy)]
pub struct TunnelSnapshot {
    pub t_wall: u64,
    pub up_bytes: u64,
    pub down_bytes: u64,
    pub active_conns: u64,
    pub total_conns: u64,
    pub connect_errors: u64,
}

impl TunnelSnapshot {
    pub fn now() -> Self {
        Self {
            t_wall: now_wall_ms(),
            up_bytes: UP_BYTES.load(Ordering::Relaxed),
            down_bytes: DOWN_BYTES.load(Ordering::Relaxed),
            active_conns: ACTIVE_CONNS.load(Ordering::Relaxed),
            total_conns: TOTAL_CONNS.load(Ordering::Relaxed),
            connect_errors: CONNECT_ERRORS.load(Ordering::Relaxed),
        }
    }
}
