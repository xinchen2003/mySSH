//! 重连退避抖动（PR-4）：equal jitter（capped/2 + random(0, capped/2)），
//! 消除多隧道/多会话断线后的同步重连（惊群）。
//!
//! 随机源为进程内 xorshift64*，仅用于退避抖动，非安全用途。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static RNG_STATE: AtomicU64 = AtomicU64::new(0);
static SEED_SEQ: AtomicU64 = AtomicU64::new(1);

fn next_u64() -> u64 {
    let mut x = RNG_STATE.load(Ordering::Relaxed);
    if x == 0 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() ^ u64::from(d.subsec_nanos()))
            .unwrap_or(0x9E37_79B9_7F4A_7C15);
        // 奇数保证 xorshift 非零；混入递增计数区分同刻多线程播种
        let seed = (nanos ^ SEED_SEQ.fetch_add(0x9E37_79B9, Ordering::Relaxed)) | 1;
        x = match RNG_STATE.compare_exchange(0, seed, Ordering::Relaxed, Ordering::Relaxed) {
            // 成功时返回的是旧值 0，必须用 seed 本身（xorshift(0)=0 会卡死序列）
            Ok(_) => seed,
            Err(cur) => cur,
        };
    }
    // xorshift64*
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    RNG_STATE.store(x, Ordering::Relaxed);
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

/// [0, 1) 均匀随机
fn next_rand01() -> f64 {
    (next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// 纯函数核心：`capped/2 + rand01 * capped/2`，结果落在 [capped/2, capped]
pub fn equal_jitter_with(capped: Duration, rand01: f64) -> Duration {
    let half = capped.mul_f64(0.5);
    half + half.mul_f64(rand01.clamp(0.0, 1.0))
}

/// equal jitter 退避：消除大量实例按相同节拍重连
pub fn equal_jitter(capped: Duration) -> Duration {
    equal_jitter_with(capped, next_rand01())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_bounded_within_half_and_cap() {
        let capped = Duration::from_secs(16);
        assert_eq!(equal_jitter_with(capped, 0.0), Duration::from_secs(8));
        assert_eq!(equal_jitter_with(capped, 1.0), capped);
        for i in 0..100 {
            let r = i as f64 / 100.0;
            let d = equal_jitter_with(capped, r);
            assert!(
                d >= Duration::from_secs(8) && d <= capped,
                "rand01={r} → {d:?}"
            );
        }
    }

    #[test]
    fn jitter_random_instances_not_synchronized() {
        let capped = Duration::from_secs(8);
        let samples: Vec<Duration> = (0..8).map(|_| equal_jitter(capped)).collect();
        assert!(
            samples.iter().any(|d| *d != samples[0]),
            "8 samples identical: jitter not working"
        );
        for d in &samples {
            assert!(*d >= Duration::from_secs(4) && *d <= capped);
        }
    }

    #[test]
    fn jitter_zero_cap_is_zero() {
        assert_eq!(equal_jitter(Duration::ZERO), Duration::ZERO);
    }
}
