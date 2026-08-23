//! 纯函数解析器：/proc 与 ps 输出 → 原始采样结构。

use crate::types::ProcInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    pub(crate) fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }
    pub(crate) fn idle_all(&self) -> u64 {
        self.idle + self.iowait
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct MemKb {
    pub total: u64,
    pub avail: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct LoadAvg {
    pub l1: f32,
    pub l5: f32,
    pub l15: f32,
    pub running: u32,
    pub total: u32,
}

#[derive(Debug, Default)]
pub(crate) struct RawSample {
    pub cpu: Option<CpuTimes>,
    pub mem: Option<MemKb>,
    pub load: Option<LoadAvg>,
    /// (设备, 读扇区, 写扇区)
    pub disks: Vec<(String, u64, u64)>,
    /// (网卡, rx字节, tx字节)
    pub nets: Vec<(String, u64, u64)>,
    pub procs: Vec<ProcInfo>,
}

/// 按 `==NAME==` 段切分组合脚本输出（两遍扫描：先定位段头，再切片）。
pub(crate) fn split_sections(out: &str) -> Vec<(&str, &str)> {
    let mut marks: Vec<(&str, usize, usize)> = Vec::new(); // (name, body_start, header_start)
    let mut pos = 0usize;
    for line in out.split_inclusive('\n') {
        if let Some(inner) = line
            .trim()
            .strip_prefix("==")
            .and_then(|s| s.strip_suffix("=="))
        {
            marks.push((inner.trim(), pos + line.len(), pos));
        }
        pos += line.len();
    }
    marks
        .iter()
        .enumerate()
        .map(|(i, (name, body_start, _))| {
            let end = marks.get(i + 1).map_or(out.len(), |m| m.2);
            (*name, &out[*body_start..end])
        })
        .collect()
}

/// 聚合行 `cpu  user nice system idle iowait irq softirq steal ...`（首行，jiffies）。
pub(crate) fn parse_cpu(body: &str) -> Option<CpuTimes> {
    let line = body.lines().find(|l| l.starts_with("cpu "))?;
    let mut it = line.split_whitespace();
    it.next()?; // "cpu"
    Some(CpuTimes {
        user: it.next()?.parse().ok()?,
        nice: it.next()?.parse().ok()?,
        system: it.next()?.parse().ok()?,
        idle: it.next()?.parse().ok()?,
        iowait: it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        irq: it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        softirq: it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
        steal: it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
    })
}

/// meminfo 的 kB 字段。
pub(crate) fn parse_meminfo(body: &str) -> Option<MemKb> {
    let get = |key: &str| -> Option<u64> {
        body.lines()
            .find_map(|l| l.strip_prefix(key)?.split_whitespace().next()?.parse().ok())
    };
    let total = get("MemTotal:")?;
    // 老内核无 MemAvailable：退化为 free+buffers+cached 近似。
    let avail = get("MemAvailable:").or_else(|| {
        Some(get("MemFree:")? + get("Buffers:").unwrap_or(0) + get("Cached:").unwrap_or(0))
    })?;
    Some(MemKb {
        total,
        avail,
        swap_total: get("SwapTotal:").unwrap_or(0),
        swap_free: get("SwapFree:").unwrap_or(0),
    })
}

/// `0.12 0.34 0.56 2/345 12345`
pub(crate) fn parse_loadavg(body: &str) -> Option<LoadAvg> {
    let line = body.lines().next()?.trim();
    let mut it = line.split_whitespace();
    let l1 = it.next()?.parse().ok()?;
    let l5 = it.next()?.parse().ok()?;
    let l15 = it.next()?.parse().ok()?;
    let (running, total) = it
        .next()
        .and_then(|s| s.split_once('/'))
        .and_then(|(r, t)| Some((r.parse().ok()?, t.parse().ok()?)))
        .unwrap_or((0, 0));
    Some(LoadAvg {
        l1,
        l5,
        l15,
        running,
        total,
    })
}

/// diskstats：`major minor name rd_ios rd_merges rd_sectors rd_ticks wr_ios wr_merges wr_sectors ...`
/// 过滤 loop*/ram*；rd_sectors=f[5], wr_sectors=f[9]。
pub(crate) fn parse_diskstats(body: &str) -> Vec<(String, u64, u64)> {
    body.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 10 {
                return None;
            }
            let name = f[2];
            if name.starts_with("loop") || name.starts_with("ram") {
                return None;
            }
            Some((name.to_string(), f[5].parse().ok()?, f[9].parse().ok()?))
        })
        .collect()
}

/// net/dev：两行表头后 `iface: rx_bytes packets ... tx_bytes ...`，rx=f[0], tx=f[8]（冒号切开后）。
pub(crate) fn parse_net_dev(body: &str) -> Vec<(String, u64, u64)> {
    body.lines()
        .skip(2)
        .filter_map(|l| {
            let (iface, rest) = l.split_once(':')?;
            let iface = iface.trim();
            if iface == "lo" {
                return None;
            }
            let f: Vec<&str> = rest.split_whitespace().collect();
            if f.len() < 9 {
                return None;
            }
            Some((iface.to_string(), f[0].parse().ok()?, f[8].parse().ok()?))
        })
        .collect()
}

/// `ps -eo pid=,rss=,pcpu=,pmem=,comm= --sort=-pcpu | head`
pub(crate) fn parse_ps(body: &str) -> Vec<ProcInfo> {
    body.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 5 {
                return None;
            }
            Some(ProcInfo {
                pid: f[0].parse().ok()?,
                rss_kb: f[1].parse().ok()?,
                cpu_pct: f[2].parse().ok()?,
                mem_pct: f[3].parse().ok()?,
                comm: f[4..].join(" "),
            })
        })
        .collect()
}

/// 整段组合输出 → RawSample。STAT/MEM 缺失时相应字段为 None（降级）。
pub(crate) fn parse_round(out: &str) -> RawSample {
    let mut s = RawSample::default();
    for (name, body) in split_sections(out) {
        match name {
            "STAT" => s.cpu = parse_cpu(body),
            "MEM" => s.mem = parse_meminfo(body),
            "LOAD" => s.load = parse_loadavg(body),
            "DISK" => s.disks = parse_diskstats(body),
            "NET" => s.nets = parse_net_dev(body),
            "PS" => s.procs = parse_ps(body),
            _ => {}
        }
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const STAT: &str =
        "cpu  4705 356 584 16205 229 0 23 0 0 0\ncpu0 1200 90 150 4000 60 0 8 0 0 0\nintr 1\n";
    const MEM: &str = "MemTotal:        3867720 kB\nMemFree:          142100 kB\nMemAvailable:    2100400 kB\nBuffers:           50000 kB\nCached:          1800000 kB\nSwapTotal:       1048572 kB\nSwapFree:        1048572 kB\n";
    const LOAD: &str = "1.25 0.90 0.40 2/345 12345\n";
    const DISK: &str = "   8       0 sda 1000 0 204800 500 2000 0 409600 800 0 0 0\n 253       0 dm-0 900 0 184320 400 1800 0 368640 700 0 0 0\n   7       0 loop0 10 0 80 5 0 0 0 0 0 0 0\n";
    const NET: &str = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n  eth0: 123456789  100000    0    0    0     0          0         0 987654321   90000    0    0    0     0       0          0\n    lo:   10000     100    0    0    0     0          0         0    10000     100    0    0    0     0       0          0\n";
    const PS: &str = " 1234 102400 12.5 2.6 java\n  100   2048  0.0 0.1 systemd\n";

    #[test]
    fn parses_cpu_and_busy_delta() {
        let a = parse_cpu(STAT).unwrap();
        assert_eq!(a.idle_all(), 16205 + 229);
        assert_eq!(a.total(), 4705 + 356 + 584 + 16205 + 229 + 23);
        // 构造 100 jiffies 后 idle+50 → 忙率 50%
        let mut b = a;
        b.idle += 50;
        b.system += 50;
        let dt = (b.total() - a.total()) as f32;
        let busy = 100.0 * (1.0 - (b.idle_all() - a.idle_all()) as f32 / dt);
        assert!((busy - 50.0).abs() < 0.01);
    }

    #[test]
    fn parses_mem_load_disk_net_ps() {
        let m = parse_meminfo(MEM).unwrap();
        assert_eq!(m.total, 3867720);
        assert_eq!(m.avail, 2100400);
        let l = parse_loadavg(LOAD).unwrap();
        assert_eq!((l.l1, l.running, l.total), (1.25, 2, 345));
        let d = parse_diskstats(DISK);
        assert_eq!(d.len(), 2); // loop0 被过滤
        assert_eq!(d[0], ("sda".to_string(), 204800, 409600));
        let n = parse_net_dev(NET);
        assert_eq!(n.len(), 1); // lo 被过滤
        assert_eq!(n[0], ("eth0".to_string(), 123456789, 987654321));
        let p = parse_ps(PS);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0].pid, 1234);
        assert_eq!(p[0].comm, "java");
    }

    #[test]
    fn sections_split_and_round() {
        let out = format!(
            "==STAT==\n{STAT}==MEM==\n{MEM}==LOAD==\n{LOAD}==DISK==\n{DISK}==NET==\n{NET}==PS==\n{PS}"
        );
        let s = parse_round(&out);
        assert!(s.cpu.is_some());
        assert!(s.mem.is_some());
        assert!(s.load.is_some());
        assert_eq!(s.disks.len(), 2);
        assert_eq!(s.nets.len(), 1);
        assert_eq!(s.procs.len(), 2);
    }

    #[test]
    fn degraded_when_sections_missing() {
        // 非 Linux：输出为空或命令报错文本 → 核心字段 None
        let s = parse_round("==STAT==\ncat: can't open '/proc/stat': No such file or directory\n");
        assert!(s.cpu.is_none());
        // meminfo 缺 MemAvailable 的老内核退化路径
        let old = "MemTotal: 1000 kB\nMemFree: 100 kB\nBuffers: 50 kB\nCached: 200 kB\n";
        let m = parse_meminfo(old).unwrap();
        assert_eq!(m.avail, 350);
    }
}
