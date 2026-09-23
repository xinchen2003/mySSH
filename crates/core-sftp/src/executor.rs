//! 传输执行器（卡 4）：`TransferQueue` 与 DirectoryJob worker 共享的编排骨架与冲突解决策略。
//!
//! ADR-0001 批准共享的是执行槽（边④）与 download_once/upload_once——编排从未被要求写两遍。
//! 本 module 拥有：暂停/取消等待 → permit 取还/复查 → 传输单元 → 错误三分
//! （cancel/pause 中断/真实失败）→ record_error 标可疑代际 → 重试计数与退避。
//! 两侧真实差异（registry 常驻 vs transient、priority 让行账、job 计数、报告去向）
//! 经 [`ExecFlow`] hooks 收口，调用方变薄。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;

use crate::transfer::{download_once, upload_once, TransferInner};
use crate::{SftpError, SftpSlot, TransferDirection, TransferState};

/// 单文件失败就地重试次数（queue 与 job 共用此唯一来源）
pub(crate) const DEFAULT_MAX_RETRIES: u32 = 2;
/// 重试退避（不持 permit）
pub(crate) const RETRY_BACKOFF: Duration = Duration::from_secs(1);

type TransferFn<'a> =
    Box<dyn FnMut(Arc<TransferInner>) -> BoxFuture<'a, Result<(), SftpError>> + Send + 'a>;
type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 编排骨架的不变部分。差异点经 [`ExecFlow`] 注入。
pub(crate) struct Executor<'a> {
    /// 执行槽预算（queue 与 job worker 共享同一账本，ADR-0001 边④）
    pub(crate) permits: &'a Arc<core_policy::Budget>,
    pub(crate) pause: &'a AtomicBool,
    pub(crate) cancel: &'a AtomicBool,
    /// 暂停唤醒：Some = Notify（job）；None = 200ms 自旋（queue 的 TransferInner 无 Notify）
    pub(crate) wake: Option<&'a Notify>,
    pub(crate) max_retries: u32,
    transfer: TransferFn<'a>,
    /// 真实失败（非 cancel/pause 中断）时标 data 代际可疑（C8）
    on_real_error: Box<dyn Fn() + Send + Sync + 'a>,
}

/// 单次尝试的终态/中间态报告（调用方各自记账）
pub(crate) enum AttemptEnd {
    /// 传输完成
    Done,
    /// 取消终态（顶部检出或传输中断后发现取消）
    Canceled,
    /// 重试耗尽的失败终态
    Failed,
    /// 非终态失败：将退避重试（queue 记 retries/error + emit；job 无操作）
    Retry,
}

pub(crate) struct AttemptReport {
    pub(crate) kind: AttemptEnd,
    /// 本轮已确认字节数（job 计入 bytes_done；queue 忽略——registry 自读）
    pub(crate) bytes: u64,
    pub(crate) error: Option<String>,
}

/// 两侧编排差异的收口 hooks。
pub(crate) trait ExecFlow {
    /// 单次尝试的账本守卫：Drop 归还本轮记账（job：active/current/watch；queue：无操作）
    type Guard;
    /// 每轮装配：本轮 TransferInner（queue 常驻复用；job transient 新建）+ 守卫；
    /// 进入 Running 的各自记账在此完成。`attempt` = 当前尝试序号（0 起，job 写入 transient info）
    fn prepare(&mut self, attempt: u32) -> (Arc<TransferInner>, Self::Guard);
    /// 循环顶部检出的取消终态（queue：registry Canceled + 还账 + emit；job：report bytes=0）
    fn cancel_terminal(&mut self);
    /// 暂停等待入口/出口（queue 入口：Paused 状态 + priority 账迁移 + emit；job：无操作）
    fn pause_edge(&mut self, _entering: bool) {}
    /// permit 前的额外让行闸（queue：priority 让行自旋 + 1→2 账迁移；job：无操作）
    fn pre_permit_gate(&mut self) -> impl Future<Output = ()> + Send + '_ {
        async {}
    }
    /// 每轮结束报告（终态与非终态重试；消费本轮守卫）
    fn report(&mut self, report: AttemptReport, guard: Self::Guard);
}

impl<'a> Executor<'a> {
    /// 生产装配：data.get() 取当前代际 client → download/upload 分发；
    /// 真实失败经 record_error 标可疑（C8：下次取用先探活、死了单飞重建）
    pub(crate) fn sftp(
        direction: TransferDirection,
        permits: &'a Arc<core_policy::Budget>,
        data: &'a Arc<SftpSlot>,
        pause: &'a AtomicBool,
        cancel: &'a AtomicBool,
        wake: Option<&'a Notify>,
        max_retries: u32,
    ) -> Self {
        Self {
            permits,
            pause,
            cancel,
            wake,
            max_retries,
            transfer: Box::new(move |t: Arc<TransferInner>| {
                Box::pin(async move {
                    match data.get().await {
                        Ok(client) => match direction {
                            TransferDirection::Download => download_once(client, t).await,
                            TransferDirection::Upload => upload_once(client, t).await,
                        },
                        Err(e) => Err(e),
                    }
                })
            }),
            on_real_error: Box::new(move || data.record_error()),
        }
    }

    #[cfg(test)]
    fn for_test(
        permits: &'a Arc<core_policy::Budget>,
        pause: &'a AtomicBool,
        cancel: &'a AtomicBool,
        max_retries: u32,
        transfer: TransferFn<'a>,
    ) -> Self {
        Self {
            permits,
            pause,
            cancel,
            wake: None,
            max_retries,
            transfer,
            on_real_error: Box::new(|| {}),
        }
    }

    /// 编排骨架。P1-8 不变量：暂停自旋与重试退避一律不持有 permit；
    /// acquire 等待期间的暂停/取消经二次确认，绝不持 permit 进传输；
    /// 执行单元结束立即还 permit，后续去向（报告/退避）均不持有它。
    pub(crate) async fn run<F>(&mut self, flow: &mut F) -> TransferState
    where
        F: ExecFlow + Send,
        F::Guard: Send,
    {
        let mut attempt = 0u32;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                flow.cancel_terminal();
                return TransferState::Canceled;
            }
            // 非暂停等待（不持 permit、不占并发额度）
            if self.pause.load(Ordering::Relaxed) {
                flow.pause_edge(true);
                match self.wake {
                    Some(wake) => {
                        while self.pause.load(Ordering::Relaxed)
                            && !self.cancel.load(Ordering::Relaxed)
                        {
                            wake.notified().await;
                        }
                    }
                    None => {
                        while self.pause.load(Ordering::Relaxed)
                            && !self.cancel.load(Ordering::Relaxed)
                        {
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
                flow.pause_edge(false);
                continue; // 回顶部：cancel 分支或重新竞争 permit
            }
            flow.pre_permit_gate().await;
            let permit = self.permits.clone().acquire().await;
            if self.cancel.load(Ordering::Relaxed) || self.pause.load(Ordering::Relaxed) {
                drop(permit);
                continue; // 交由顶部 cancel/pause 分支
            }
            let (t, guard) = flow.prepare(attempt);
            let result = (self.transfer)(t.clone()).await;
            drop(permit);
            let bytes = t.final_bytes();
            match result {
                Ok(()) => {
                    flow.report(
                        AttemptReport {
                            kind: AttemptEnd::Done,
                            bytes,
                            error: None,
                        },
                        guard,
                    );
                    return TransferState::Done;
                }
                Err(e) => {
                    if self.cancel.load(Ordering::Relaxed) {
                        flow.report(
                            AttemptReport {
                                kind: AttemptEnd::Canceled,
                                bytes,
                                error: Some(e.to_string()),
                            },
                            guard,
                        );
                        return TransferState::Canceled;
                    }
                    if self.pause.load(Ordering::Relaxed) {
                        // 暂停引发的断点中断不算失败：回顶部等恢复（不占 permit、不计重试）
                        drop(guard);
                        continue;
                    }
                    (self.on_real_error)();
                    attempt += 1;
                    let error = Some(e.to_string());
                    if attempt > self.max_retries {
                        flow.report(
                            AttemptReport {
                                kind: AttemptEnd::Failed,
                                bytes,
                                error,
                            },
                            guard,
                        );
                        return TransferState::Failed;
                    }
                    flow.report(
                        AttemptReport {
                            kind: AttemptEnd::Retry,
                            bytes,
                            error,
                        },
                        guard,
                    );
                    tokio::time::sleep(RETRY_BACKOFF).await;
                }
            }
        }
    }
}

/// 远端目标冲突解析：Ok(None) = skip；Ok(Some((最终路径, 运行期模式))) = 执行。
/// probe 注入存在性探测（Ok(true) = 目标存在）——调用方选 slot
/// （单文件 enqueue 前走 metadata；job worker 执行时走 data，时机差异见 ADR-0001 决策 6）
pub async fn resolve_remote<F, Fut>(
    probe: F,
    target: &str,
    policy: crate::OnExists,
) -> Result<Option<(String, OnExists)>, SftpError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<bool, SftpError>>,
{
    if !probe(target.to_string()).await? {
        // 不存在（或不可 stat）：直接执行，运行期续传逻辑自负盈亏
        return Ok(Some((target.to_string(), policy.runtime())));
    }
    match policy {
        OnExists::Resume | OnExists::Overwrite => Ok(Some((target.to_string(), policy))),
        OnExists::Skip => Ok(None),
        OnExists::Rename => {
            for n in 1..1000 {
                let cand = crate::rename_candidate(target, n);
                if !probe(cand.clone()).await? {
                    return Ok(Some((cand, OnExists::Resume)));
                }
            }
            Err(SftpError::RemotePath {
                path: target.to_string(),
                reason: "自动改名失败: name-N 候选均被占用".into(),
            })
        }
    }
}

/// 本地目标冲突解析（与远端同策略；存在性看 std::fs）
pub fn resolve_local(
    target: &Path,
    policy: OnExists,
) -> Result<Option<(PathBuf, OnExists)>, SftpError> {
    if !target.exists() {
        return Ok(Some((target.to_path_buf(), policy.runtime())));
    }
    match policy {
        OnExists::Resume | OnExists::Overwrite => Ok(Some((target.to_path_buf(), policy))),
        OnExists::Skip => Ok(None),
        OnExists::Rename => {
            let s = target.to_string_lossy();
            for n in 1..1000 {
                let cand = crate::rename_candidate(&s, n);
                if !Path::new(&cand).exists() {
                    return Ok(Some((PathBuf::from(cand), OnExists::Resume)));
                }
            }
            Err(SftpError::LocalIo {
                path: target.display().to_string(),
                reason: "自动改名失败: name-N 候选均被占用".into(),
            })
        }
    }
}

use crate::OnExists;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use parking_lot::Mutex;

    use super::*;

    /// 计数 flow：报告全收集，无任何账本副作用
    struct FakeFlow {
        t: Arc<TransferInner>,
        reports: Vec<AttemptEndKind>,
    }

    #[derive(Debug, PartialEq, Clone, Copy)]
    enum AttemptEndKind {
        Done,
        Canceled,
        Failed,
        Retry,
        CancelTop,
    }

    impl ExecFlow for FakeFlow {
        type Guard = ();
        fn prepare(&mut self, _attempt: u32) -> (Arc<TransferInner>, ()) {
            (self.t.clone(), ())
        }
        fn cancel_terminal(&mut self) {
            self.reports.push(AttemptEndKind::CancelTop);
        }
        fn report(&mut self, report: AttemptReport, (): ()) {
            self.reports.push(match report.kind {
                AttemptEnd::Done => AttemptEndKind::Done,
                AttemptEnd::Canceled => AttemptEndKind::Canceled,
                AttemptEnd::Failed => AttemptEndKind::Failed,
                AttemptEnd::Retry => AttemptEndKind::Retry,
            });
        }
    }

    fn fake_inner() -> Arc<TransferInner> {
        TransferInner::new_transient(crate::TransferInfo {
            id: "t1".into(),
            direction: TransferDirection::Download,
            local: PathBuf::from("/tmp/x"),
            remote: "/r/x".into(),
            state: TransferState::Running,
            bytes_done: 0,
            bytes_total: 10,
            on_exists: OnExists::Resume,
            priority: false,
            retries: 0,
            error: None,
        })
    }

    fn fake_err() -> SftpError {
        SftpError::LocalIo {
            path: "x".into(),
            reason: "boom".into(),
        }
    }

    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    /// 真实失败重试到上限：Retry×2（退避 1s×2）→ Failed 终态
    #[tokio::test(start_paused = true)]
    async fn retries_up_to_limit_then_failed() {
        let permits = Arc::new(core_policy::Budget::new("test.exec", 1));
        let pause = AtomicBool::new(false);
        let cancel = AtomicBool::new(false);
        let calls = Arc::new(Mutex::new(0u32));
        let calls2 = calls.clone();
        let mut ex = Executor::for_test(
            &permits,
            &pause,
            &cancel,
            DEFAULT_MAX_RETRIES,
            Box::new(move |_t| {
                *calls2.lock() += 1;
                Box::pin(async { Err(fake_err()) })
            }),
        );
        let mut flow = FakeFlow {
            t: fake_inner(),
            reports: Vec::new(),
        };
        let driver = async {
            settle().await; // attempt 0 失败，进入退避
            tokio::time::advance(RETRY_BACKOFF).await;
            settle().await; // attempt 1 失败，进入退避
            tokio::time::advance(RETRY_BACKOFF).await;
            settle().await; // attempt 2 失败 → Failed
                            // 兜底推进防挂（断言先行失败时也不留悬挂）
            tokio::time::advance(Duration::from_secs(10)).await;
            settle().await;
        };
        let (state, ()) = tokio::join!(ex.run(&mut flow), driver);
        assert_eq!(state, TransferState::Failed);
        assert_eq!(*calls.lock(), 3, "1 首发 + 2 重试");
        assert_eq!(
            flow.reports,
            [
                AttemptEndKind::Retry,
                AttemptEndKind::Retry,
                AttemptEndKind::Failed
            ]
        );
    }

    /// 暂停引发的断点中断不算失败：不计重试、无 Retry 报告，恢复后从断点重跑成功
    #[tokio::test(start_paused = true)]
    async fn pause_interrupt_is_not_a_failure() {
        let permits = Arc::new(core_policy::Budget::new("test.exec", 1));
        let pause = Arc::new(AtomicBool::new(false));
        let cancel = AtomicBool::new(false);
        let p2 = pause.clone();
        let calls = Arc::new(Mutex::new(0u32));
        let calls2 = calls.clone();
        let mut ex = Executor::for_test(
            &permits,
            &pause,
            &cancel,
            DEFAULT_MAX_RETRIES,
            Box::new(move |t: Arc<TransferInner>| {
                *calls2.lock() += 1;
                let n = *calls2.lock();
                let p3 = p2.clone();
                Box::pin(async move {
                    if n == 1 {
                        // 模拟 chunk 边界中断：暂停位置位，传输中断报错
                        p3.store(true, Ordering::Relaxed);
                        t.signal(true, false);
                        Err(fake_err())
                    } else {
                        Ok(())
                    }
                })
            }),
        );
        let mut flow = FakeFlow {
            t: fake_inner(),
            reports: Vec::new(),
        };
        let driver = async {
            settle().await; // 首次中断 → 回顶部进暂停自旋（200ms 粒度）
            tokio::time::advance(Duration::from_millis(600)).await;
            settle().await;
            assert_eq!(*calls.lock(), 1, "暂停中不得重试/终结");
            pause.store(false, Ordering::Relaxed);
            tokio::time::advance(Duration::from_millis(400)).await;
            settle().await;
        };
        let (state, ()) = tokio::join!(ex.run(&mut flow), driver);
        assert_eq!(state, TransferState::Done);
        assert_eq!(*calls.lock(), 2, "恢复后从断点重跑一次成功");
        assert_eq!(flow.reports, [AttemptEndKind::Done], "中断不计 Retry");
    }

    /// 顶部取消：立即终态、零传输调用
    #[tokio::test(start_paused = true)]
    async fn cancel_before_start_is_terminal() {
        let permits = Arc::new(core_policy::Budget::new("test.exec", 1));
        let pause = AtomicBool::new(false);
        let cancel = AtomicBool::new(true);
        let calls = Arc::new(Mutex::new(0u32));
        let calls2 = calls.clone();
        let mut ex = Executor::for_test(
            &permits,
            &pause,
            &cancel,
            DEFAULT_MAX_RETRIES,
            Box::new(move |_t| {
                *calls2.lock() += 1;
                Box::pin(async { Ok(()) })
            }),
        );
        let mut flow = FakeFlow {
            t: fake_inner(),
            reports: Vec::new(),
        };
        let state = ex.run(&mut flow).await;
        assert_eq!(state, TransferState::Canceled);
        assert_eq!(*calls.lock(), 0);
    }
}
