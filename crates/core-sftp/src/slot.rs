//! SFTP subsystem 单飞重建槽（性能优化 PR-11 / 约束 C8）。
//!
//! 代际结构：`TransportGeneration ├─ MetadataGeneration └─ DataGeneration`。
//! Transport 代际由 app 层 ensure_ctx 承载（连接死 → 整个 ctx 连同两 slot 重建）；
//! 本类型只负责单 subsystem 代际：
//! - 操作失败方 `record_error` 标可疑；下次取用先探活（probe），探活通过保留
//!   原代际（远端业务错误如权限不足不会引发重建），探活失败才重建（新代际）。
//! - 恢复任务（探活/重连）由 slot 自己 spawn 持有，首个 waiter 被取消不连累
//!   其余 waiter（禁止裸 OnceCell，连接不归首个调用者的 Future 所有）。
//! - 单飞：同一时刻至多一个 Recovering；并发取用者挂同一 watch 通道等结果。
//! - 代际不串扰：进入 Recovering 的转换在锁内完成，完成者是唯一能写状态的
//!   任务；新代际就绪前旧代际 client 不会被替换，旧代际失败也不覆盖新状态。

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::{SftpClient, SftpError};

/// 锁中毒自愈（panic 现场已恢复，状态机本身无损坏语义）
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// 工厂：开一个新 subsystem client（失败原因进 SftpError）
pub type SlotFactory<C> = Arc<dyn Fn() -> BoxFut<Result<C, SftpError>> + Send + Sync>;
/// 探活：对既有 client 发一个廉价只读请求（如 REALPATH "."）
pub type SlotProbe<C> = Arc<dyn Fn(Arc<C>) -> BoxFut<Result<(), SftpError>> + Send + Sync>;

type SlotOutcome<C> = Result<Arc<C>, String>;

enum SlotState<C> {
    /// 无 client（初始 / 上次重建失败且未设退避）：下次取用发起新代际连接
    Empty,
    /// 上次连接失败 + 退避窗口（PR-12：不缓存失败，窗口内直接报错防连续捶打死服务）
    Failed {
        at: std::time::Instant,
        error: String,
    },
    /// 恢复中（首开/探活/重建）：结果经 watch 广播给全部 waiter
    Recovering {
        rx: tokio::sync::watch::Receiver<Option<SlotOutcome<C>>>,
    },
    Ready {
        generation: u64,
        client: Arc<C>,
    },
}

/// subsystem 单飞槽（泛型便于无 SSH 的状态机单测；生产用 [`SftpSlot`]）
pub struct SubsystemSlot<C> {
    name: &'static str,
    factory: SlotFactory<C>,
    probe: SlotProbe<C>,
    rt: tokio::runtime::Handle,
    state: Mutex<SlotState<C>>,
    /// 可疑标记：任一操作失败后置位；下次取用触发探活
    suspect: AtomicBool,
    seq: AtomicU64,
    /// 连接失败退避窗口毫秒（0 = 立即重试；PR-12 ctx 级槽设 2s）
    retry_backoff: AtomicU64,
}

impl<C: Send + Sync + 'static> SubsystemSlot<C> {
    /// 空槽起步（首开推迟到首次取用）；生产路径用后即 ensure，失败即报错
    pub fn new(
        name: &'static str,
        rt: tokio::runtime::Handle,
        factory: SlotFactory<C>,
        probe: SlotProbe<C>,
    ) -> Arc<Self> {
        Arc::new(Self {
            name,
            factory,
            probe,
            rt,
            state: Mutex::new(SlotState::Empty),
            suspect: AtomicBool::new(false),
            seq: AtomicU64::new(0),
            retry_backoff: AtomicU64::new(0),
        })
    }

    /// 连接失败退避窗口（PR-12 ctx 级槽；默认 0 = 失败后可立即重试）
    pub fn set_retry_backoff(&self, d: std::time::Duration) {
        self.retry_backoff
            .store(d.as_millis() as u64, Ordering::Relaxed);
    }

    /// 当前 Ready client（无则 None；不触发恢复）
    pub fn ready(&self) -> Option<Arc<C>> {
        match &*lock(&self.state) {
            SlotState::Ready { client, .. } => Some(client.clone()),
            _ => None,
        }
    }

    /// 主动失效（如底层 Transport 已断）：Ready → Empty，下次取用重建；
    /// Recovering/Failed 不动（在途恢复不被打断，退避语义保留）
    pub fn invalidate(&self) {
        let mut st = lock(&self.state);
        if matches!(&*st, SlotState::Ready { .. }) {
            *st = SlotState::Empty;
        }
    }

    /// 槽状态名（perf 观测用）
    pub fn state_name(&self) -> &'static str {
        match &*lock(&self.state) {
            SlotState::Empty => "empty",
            SlotState::Failed { .. } => "failed",
            SlotState::Recovering { .. } => "connecting",
            SlotState::Ready { .. } => "ready",
        }
    }

    fn backoff(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.retry_backoff.load(Ordering::Relaxed))
    }

    /// 操作失败上报：标可疑，下次取用先探活再决定保留/重建
    pub fn record_error(&self) {
        self.suspect.store(true, Ordering::Release);
    }

    /// 当前代际（诊断/测试断言用；Empty/Recovering 期间返回已分配的最大代际）
    pub fn generation(&self) -> u64 {
        match &*lock(&self.state) {
            SlotState::Ready { generation, .. } => *generation,
            _ => self.seq.load(Ordering::Relaxed),
        }
    }

    /// 取当前可用 client：快路径直接克隆；可疑/空缺时单飞恢复
    pub async fn get(self: &Arc<Self>) -> Result<Arc<C>, SftpError> {
        {
            let st = lock(&self.state);
            if let SlotState::Ready { client, .. } = &*st {
                if !self.suspect.load(Ordering::Acquire) {
                    return Ok(client.clone());
                }
            }
        }
        self.ensure().await
    }

    async fn ensure(self: &Arc<Self>) -> Result<Arc<C>, SftpError> {
        enum Act<C> {
            Return(Arc<C>),
            Recover(Option<(u64, Arc<C>)>),
            Wait(tokio::sync::watch::Receiver<Option<SlotOutcome<C>>>),
            Fail(String),
        }
        let mut rx = {
            let mut st = lock(&self.state);
            let act = match &*st {
                SlotState::Ready { client, .. } if !self.suspect.load(Ordering::Acquire) => {
                    Act::Return(client.clone())
                }
                SlotState::Ready { generation, client } => {
                    Act::Recover(Some((*generation, client.clone())))
                }
                SlotState::Empty => Act::Recover(None),
                SlotState::Failed { at, error } => {
                    if at.elapsed() < self.backoff() {
                        Act::Fail(error.clone())
                    } else {
                        Act::Recover(None)
                    }
                }
                SlotState::Recovering { rx } => Act::Wait(rx.clone()),
            };
            match act {
                Act::Return(c) => return Ok(c),
                Act::Recover(old) => {
                    // 单飞：锁内转换，并发者只会看到 Recovering 并挂同一 watch
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    *st = SlotState::Recovering { rx: rx.clone() };
                    self.spawn_recover(old, tx);
                    rx
                }
                Act::Wait(rx) => rx,
                Act::Fail(e) => return Err(SftpError::Subsystem(e)),
            }
        };
        loop {
            if let Some(outcome) = rx.borrow().clone() {
                return outcome.map_err(SftpError::Subsystem);
            }
            rx.changed().await.map_err(|_| {
                SftpError::Subsystem(format!("{} subsystem 恢复任务丢失", self.name))
            })?;
        }
    }

    /// 恢复任务：由 slot 持有（waiter 取消不影响），完成者唯一写状态
    fn spawn_recover(
        self: &Arc<Self>,
        old: Option<(u64, Arc<C>)>,
        tx: tokio::sync::watch::Sender<Option<SlotOutcome<C>>>,
    ) {
        let me = Arc::clone(self);
        self.rt.spawn(async move {
            // kept: 探活通过保留原代际；fresh: 新代际
            enum Done<C> {
                Kept(u64, Arc<C>),
                Fresh(Arc<C>),
                Failed(String),
            }
            let done = async {
                if let Some((gen, old_client)) = old {
                    if (me.probe)(old_client.clone()).await.is_ok() {
                        return Done::Kept(gen, old_client);
                    }
                }
                match (me.factory)().await {
                    Ok(c) => Done::Fresh(Arc::new(c)),
                    Err(e) => Done::Failed(e.to_string()),
                }
            }
            .await;
            // 单飞不变量：同一时刻只有本任务处于 Recovering（转换在锁内完成，
            // 其余取用者只克隆 rx 不写状态），故此处直接落定终态
            let mut st = lock(&me.state);
            debug_assert!(matches!(&*st, SlotState::Recovering { .. }));
            match done {
                Done::Kept(gen, client) => {
                    me.suspect.store(false, Ordering::Release);
                    *st = SlotState::Ready {
                        generation: gen,
                        client: client.clone(),
                    };
                    let _ = tx.send(Some(Ok(client)));
                }
                Done::Fresh(client) => {
                    me.suspect.store(false, Ordering::Release);
                    let gen = me.seq.fetch_add(1, Ordering::Relaxed) + 1;
                    *st = SlotState::Ready {
                        generation: gen,
                        client: client.clone(),
                    };
                    let _ = tx.send(Some(Ok(client)));
                }
                Done::Failed(e) => {
                    let backoff = me.backoff();
                    if backoff.is_zero() {
                        *st = SlotState::Empty;
                    } else {
                        *st = SlotState::Failed {
                            at: std::time::Instant::now(),
                            error: e.clone(),
                        };
                    }
                    let _ = tx.send(Some(Err(e)));
                }
            }
        });
    }
}

/// 生产型：SFTP subsystem 槽
pub type SftpSlot = SubsystemSlot<SftpClient>;

impl SubsystemSlot<SftpClient> {
    /// 开槽并立即建立首个 subsystem（ensure_ctx 语义：开不起来直接报错）
    pub async fn open_sftp(
        name: &'static str,
        conn: Arc<core_ssh::SshConnection>,
        rt: tokio::runtime::Handle,
    ) -> Result<Arc<Self>, SftpError> {
        let slot = Self::new(
            name,
            rt,
            Arc::new(move || {
                let conn = conn.clone();
                Box::pin(async move { SftpClient::open(&conn).await })
            }),
            // REALPATH 是 SFTP v3 基础请求，所有服务器可用；只读、廉价
            Arc::new(|c| Box::pin(async move { c.canonicalize(".").await.map(|_| ()) })),
        );
        slot.get().await?;
        Ok(slot)
    }
}

#[cfg(test)]
// 测试代码按 workspace 惯例豁免 unwrap/expect（tests/ 目录同款 #![allow]）
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Fake {
        connects: AtomicUsize,
        probes: AtomicUsize,
        fail_connect: AtomicBool,
        fail_probe: AtomicBool,
        /// 阻塞首开直至放行（waiter 取消测试用）
        gate: Option<Arc<tokio::sync::Notify>>,
    }

    fn fake_slot(gate: Option<Arc<tokio::sync::Notify>>) -> (Arc<SubsystemSlot<u64>>, Arc<Fake>) {
        let fake = Arc::new(Fake {
            connects: AtomicUsize::new(0),
            probes: AtomicUsize::new(0),
            fail_connect: AtomicBool::new(false),
            fail_probe: AtomicBool::new(false),
            gate,
        });
        let f = fake.clone();
        let factory: SlotFactory<u64> = Arc::new(move || {
            let f = f.clone();
            Box::pin(async move {
                f.connects.fetch_add(1, Ordering::Relaxed);
                if let Some(g) = &f.gate {
                    g.notified().await;
                }
                if f.fail_connect.load(Ordering::Relaxed) {
                    Err(SftpError::Subsystem("fake connect fail".into()))
                } else {
                    Ok(7u64)
                }
            })
        });
        let f = fake.clone();
        let probe: SlotProbe<u64> = Arc::new(move |_c| {
            let f = f.clone();
            Box::pin(async move {
                f.probes.fetch_add(1, Ordering::Relaxed);
                if f.fail_probe.load(Ordering::Relaxed) {
                    Err(SftpError::Subsystem("fake probe fail".into()))
                } else {
                    Ok(())
                }
            })
        });
        (
            SubsystemSlot::new("fake", tokio::runtime::Handle::current(), factory, probe),
            fake,
        )
    }

    #[tokio::test]
    async fn empty_singleflight_ten_waiters_one_connect() {
        let (slot, fake) = fake_slot(None);
        let mut hs = Vec::new();
        for _ in 0..10 {
            let s = slot.clone();
            hs.push(tokio::spawn(async move { s.get().await }));
        }
        for h in hs {
            assert_eq!(*h.await.unwrap().unwrap(), 7);
        }
        assert_eq!(fake.connects.load(Ordering::Relaxed), 1);
        assert_eq!(slot.generation(), 1);
    }

    #[tokio::test]
    async fn suspect_probe_ok_keeps_generation() {
        let (slot, fake) = fake_slot(None);
        slot.get().await.unwrap();
        let gen = slot.generation();
        slot.record_error();
        slot.get().await.unwrap();
        assert_eq!(fake.probes.load(Ordering::Relaxed), 1);
        assert_eq!(fake.connects.load(Ordering::Relaxed), 1, "探活通过不得重连");
        assert_eq!(slot.generation(), gen, "探活通过保留原代际");
    }

    #[tokio::test]
    async fn suspect_probe_fail_rebuilds_new_generation() {
        let (slot, fake) = fake_slot(None);
        slot.get().await.unwrap();
        let gen = slot.generation();
        fake.fail_probe.store(true, Ordering::Relaxed);
        slot.record_error();
        slot.get().await.unwrap();
        assert_eq!(fake.connects.load(Ordering::Relaxed), 2);
        assert_eq!(slot.generation(), gen + 1);
    }

    #[tokio::test]
    async fn concurrent_rebuild_singleflight() {
        let (slot, fake) = fake_slot(None);
        slot.get().await.unwrap();
        fake.fail_probe.store(true, Ordering::Relaxed);
        slot.record_error();
        let mut hs = Vec::new();
        for _ in 0..10 {
            let s = slot.clone();
            hs.push(tokio::spawn(async move { s.get().await }));
        }
        for h in hs {
            h.await.unwrap().unwrap();
        }
        assert_eq!(fake.connects.load(Ordering::Relaxed), 2, "并发重建只连一次");
    }

    #[tokio::test]
    async fn factory_failure_surfaces_then_recovers() {
        let (slot, fake) = fake_slot(None);
        fake.fail_connect.store(true, Ordering::Relaxed);
        assert!(slot.get().await.is_err());
        fake.fail_connect.store(false, Ordering::Relaxed);
        slot.get().await.unwrap();
        assert_eq!(fake.connects.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn first_waiter_cancel_others_still_succeed() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let (slot, _fake) = fake_slot(Some(gate.clone()));
        let s = slot.clone();
        let h = tokio::spawn(async move { s.get().await });
        // 等首开任务进入 gate 等待后弃掉首个 waiter：连接任务由 slot 持有，不受影响
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        h.abort();
        let s = slot.clone();
        let h2 = tokio::spawn(async move { s.get().await });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        gate.notify_waiters();
        assert_eq!(*h2.await.unwrap().unwrap(), 7);
    }

    #[tokio::test]
    async fn failed_backoff_suppresses_then_allows_retry() {
        let (slot, fake) = fake_slot(None);
        slot.set_retry_backoff(std::time::Duration::from_millis(50));
        fake.fail_connect.store(true, Ordering::Relaxed);
        assert!(slot.get().await.is_err());
        assert_eq!(fake.connects.load(Ordering::Relaxed), 1);
        assert_eq!(slot.state_name(), "failed");
        // 退避窗口内：直接报错不重连（不捶打死服务）
        assert!(slot.get().await.is_err());
        assert_eq!(fake.connects.load(Ordering::Relaxed), 1);
        // 窗口过后允许重连
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        fake.fail_connect.store(false, Ordering::Relaxed);
        slot.get().await.unwrap();
        assert_eq!(fake.connects.load(Ordering::Relaxed), 2);
        assert_eq!(slot.state_name(), "ready");
    }

    #[tokio::test]
    async fn invalidate_ready_forces_new_generation() {
        let (slot, fake) = fake_slot(None);
        slot.get().await.unwrap();
        let gen = slot.generation();
        slot.invalidate();
        assert_eq!(slot.state_name(), "empty");
        slot.get().await.unwrap();
        assert_eq!(fake.connects.load(Ordering::Relaxed), 2);
        assert_eq!(slot.generation(), gen + 1);
    }

    #[tokio::test]
    async fn ready_accessor_does_not_trigger_recover() {
        let (slot, fake) = fake_slot(None);
        assert!(slot.ready().is_none());
        assert_eq!(
            fake.connects.load(Ordering::Relaxed),
            0,
            "ready() 不得触发连接"
        );
        slot.get().await.unwrap();
        assert!(slot.ready().is_some());
    }
}
