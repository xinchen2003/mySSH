//! 资源预算账本（PR-17 Governor 一期）：统一"原子扣减 + Notify 等待唤醒 + RAII 归还"。
//!
//! 一致性模型（docs/governor-子设计.md §3）：
//! - 扣减用 `fetch_update` CAS，禁止"先查再扣"两步（多并发竞态根因）；
//! - 等待挂 `Notify`，释放时 `notify_waiters` 唤醒——禁止轮询；
//! - `Permit` RAII：Drop 即归还并唤醒等待者，泄漏在类型层面不可能。

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

/// 预算快照（perf_json 指标出口）
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetSnapshot {
    pub name: String,
    pub cap: usize,
    pub active: usize,
    pub rejected: u64,
}

/// 预算耗尽（含现场，错误信息可直接给用户）
#[derive(Debug, Clone)]
pub struct BudgetExhausted {
    pub name: String,
    pub active: usize,
    pub cap: usize,
}

impl std::fmt::Display for BudgetExhausted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "budget exhausted: {} active={}/{}",
            self.name, self.active, self.cap
        )
    }
}

impl std::error::Error for BudgetExhausted {}

/// Governor 资源表上限（PR-17；docs/governor-子设计.md §2 的单一事实源——
/// 改上限只改这里，各子系统构造处引用）
pub mod caps {
    /// Tunnel channel（每 Tunnel Transport，PR-15 值；C10 预留多 Transport 语义）
    pub const TUNNEL_CHANNEL: usize = 256;
    /// SFTP 执行槽（每 ctx，TransferQueue 与 DirectoryJob 共享）
    pub const SFTP_EXEC: usize = 3;
    /// SFTP ready task（每 job 扫描结果通道；Scanner await 背压）
    pub const SFTP_READY: usize = 512;
}

/// 一类资源的预算账本（硬上限 + 超时拒绝 + 唤醒）
#[derive(Debug)]
pub struct Budget {
    name: String,
    cap: usize,
    used: AtomicUsize,
    rejected: AtomicU64,
    waiters: Notify,
}

/// 持份 guard：Drop 归还并唤醒等待者
#[derive(Debug)]
pub struct Permit {
    budget: Arc<Budget>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.budget.used.fetch_sub(1, Ordering::Relaxed);
        // 归还即唤醒一个等待者。必须 notify_one 而非 notify_waiters：
        // notify_one 在无等待者时留存一个许可，晚注册的 waiter 仍会被唤醒；
        // notify_waiters 不存许可，"CAS 失败→注册 notified() 之间发生释放"
        // 会丢失唤醒导致永久挂起（Missed wakeup）。
        self.budget.waiters.notify_one();
    }
}

impl Budget {
    pub fn new(name: impl Into<String>, cap: usize) -> Arc<Self> {
        Arc::new(Self {
            name: name.into(),
            cap,
            used: AtomicUsize::new(0),
            rejected: AtomicU64::new(0),
            waiters: Notify::new(),
        })
    }

    /// 立即尝试；满即拒（记 rejected）
    pub fn try_acquire(self: &Arc<Self>) -> Result<Permit, BudgetExhausted> {
        self.cas_take()
            .map(|_| Permit {
                budget: self.clone(),
            })
            .map_err(|active| {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                BudgetExhausted {
                    name: self.name.clone(),
                    active,
                    cap: self.cap,
                }
            })
    }

    /// 等待至多 timeout；超时记 rejected 并返回耗尽错误。永不轮询。
    pub async fn acquire_timeout(
        self: &Arc<Self>,
        timeout: Duration,
    ) -> Result<Permit, BudgetExhausted> {
        let wait = async {
            loop {
                if let Ok(p) = self.try_acquire_no_reject() {
                    return p;
                }
                self.waiters.notified().await;
            }
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(p) => Ok(p),
            Err(_) => {
                self.rejected.fetch_add(1, Ordering::Relaxed);
                Err(BudgetExhausted {
                    name: self.name.clone(),
                    active: self.used.load(Ordering::Relaxed),
                    cap: self.cap,
                })
            }
        }
    }

    /// 无限等待（背压语义，如 SFTP exec 执行槽）
    pub async fn acquire(self: &Arc<Self>) -> Permit {
        loop {
            if let Ok(p) = self.try_acquire_no_reject() {
                return p;
            }
            self.waiters.notified().await;
        }
    }

    fn try_acquire_no_reject(self: &Arc<Self>) -> Result<Permit, ()> {
        match self.cas_take() {
            Ok(()) => Ok(Permit {
                budget: self.clone(),
            }),
            Err(_) => Err(()),
        }
    }

    /// CAS 扣减：Ok(()) 或 Err(当前用量)
    fn cas_take(&self) -> Result<(), usize> {
        self.used
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |u| {
                if u < self.cap {
                    Some(u + 1)
                } else {
                    None
                }
            })
            .map(|_| ())
            .map_err(|_| self.used.load(Ordering::Relaxed))
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            name: self.name.clone(),
            cap: self.cap,
            active: self.used.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn try_acquire_rejects_at_cap_and_recovers_on_drop() {
        let b = Budget::new("t", 2);
        let p1 = b.try_acquire().unwrap();
        let p2 = b.try_acquire().unwrap();
        let err = b.try_acquire().unwrap_err();
        assert_eq!(err.active, 2);
        assert_eq!(b.snapshot().rejected, 1);
        drop(p1);
        let _p3 = b.try_acquire().unwrap();
        drop(p2);
        drop(_p3);
        assert_eq!(b.snapshot().active, 0);
    }

    #[tokio::test]
    async fn acquire_timeout_wakes_on_release_no_polling() {
        let b = Budget::new("t", 1);
        let p = b.acquire_timeout(Duration::from_secs(60)).await.unwrap();
        let b2 = b.clone();
        let waiter = tokio::spawn(async move { b2.acquire_timeout(Duration::from_secs(60)).await });
        tokio::task::yield_now().await;
        drop(p); // 释放即唤醒
        let got = waiter.await.unwrap();
        assert!(got.is_ok(), "等待者必须被释放唤醒而非超时");
        assert_eq!(b.snapshot().rejected, 0);
    }

    #[tokio::test]
    async fn acquire_timeout_rejects_after_deadline() {
        let b = Budget::new("tunnel.chan", 1);
        let _p = b.try_acquire().unwrap();
        let err = b
            .acquire_timeout(Duration::from_millis(50))
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "budget exhausted: tunnel.chan active=1/1");
        assert_eq!(b.snapshot().rejected, 1);
    }
}
