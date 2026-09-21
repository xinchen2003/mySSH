//! DirectoryTransferJob 后端调度器（ADR 0001；性能优化 PR-8；C2 单协调器防死锁）。
//!
//! 防死锁（C2）三不变量：
//! 1. 每 job 一个 ScanCoordinator，是 frontier/held/ready 的唯一协调方；
//! 2. 每个 list op 恰好发一条终态消息（Listed/DirFailed），结果通道容量 = 在途上限
//!    → op 发送永不阻塞 → op 必完成；
//! 3. 在途槽按"op 发送完"归还（共享原子计数），不按"协调器收到"归还——否则
//!    held 满会卡住归还链，重新引入死锁。配套：扫描完结判定必须同时确认
//!    **结果通道已排空**（op 先 send 后归还槽位，槽位归零时消息可能仍在通道里）。
//!
//! 内存 O(frontier + (held+通道) × 单目录 listing + ready + 活跃传输)，C2 明确允许
//! 单目录 listing 峰值；真正的流式 paged readdir 随 PR-11 metadata 通道落地。

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{mpsc, Notify, Semaphore};

use crate::transfer::{download_once, upload_once, TransferInner};
use crate::{
    EntryKind, OnExists, SftpError, SftpSlot, TransferDirection, TransferId, TransferInfo,
    TransferState,
};

/// 调度容量（生产默认；测试用 `with_caps` 缩小以压测边界）
#[derive(Debug, Clone, Copy)]
pub struct SchedulerCaps {
    /// 待扫描目录 frontier 上界
    pub frontier: usize,
    /// ready queue（TransferTask）上界
    pub ready: usize,
    /// 在途 list operation 上界（结果通道与 held 容量同值）
    pub in_flight: usize,
    /// 目录深度防护上限
    pub max_depth: u32,
    /// registry 内失败条目驻留上限（溢出只计数）
    pub failed_entries: usize,
}

impl Default for SchedulerCaps {
    fn default() -> Self {
        Self {
            frontier: 1024,
            ready: 512,
            in_flight: 4,
            max_depth: 64,
            failed_entries: 256,
        }
    }
}

/// 单条路径长度防护（字节；ADR 决策 7）
const MAX_PATH: usize = 1024;
/// 每 job 执行 worker 数（与 TransferQueue.permits 共享执行槽预算）
const WORKERS: usize = 3;
/// 单文件失败就地重试次数（对齐 TransferQueue.max_retries）
const MAX_FILE_RETRIES: u32 = 2;
/// 放置停滞时的保底轮询间隔（ready 容量释放无通知机制）
const REPOLLS_MS: u64 = 50;

/// job 状态机：Scanning（扫描与传输叠加）→ Transferring → Finalizing → Completed；
/// Failed 预留致命错误；Canceled 由 cancel 直达。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Scanning,
    Transferring,
    Finalizing,
    Completed,
    Failed,
    Canceled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scanning => "scanning",
            Self::Transferring => "transferring",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Canceled)
    }
}

/// 失败条目（registry 内有界驻留，溢出只计数）
#[derive(Debug, Clone)]
pub struct FailedEntry {
    pub path: String,
    pub error: String,
}

/// job 可观测快照（IPC 投影的权威来源）
#[derive(Debug, Clone)]
pub struct JobSnapshot {
    pub id: String,
    pub direction: TransferDirection,
    /// 展示用根路径描述（local → remote）
    pub summary: String,
    pub state: JobState,
    pub paused: bool,
    pub scan_done: bool,
    pub discovered_files: u64,
    pub discovered_bytes: u64,
    pub completed_files: u64,
    pub failed_files: u64,
    pub skipped: u64,
    pub bytes_done: u64,
    pub error: Option<String>,
    /// 在途文件（≤WORKERS 条，展示用）
    pub current: Vec<String>,
    pub failed_entries: Vec<FailedEntry>,
}

/// 待展开的根（上传：local 目录 → remote 目录；下载：remote 目录 → local 目录）
#[derive(Debug, Clone)]
pub struct JobRoot {
    pub local: PathBuf,
    pub remote: String,
}

/// 提交参数；policy 为用户冲突策略（worker 逐文件解析运行期语义）
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub direction: TransferDirection,
    pub roots: Vec<JobRoot>,
    pub policy: OnExists,
}

/// 逐文件终态记录（SQLite history writer 的输入；worker fire-and-forget）
#[derive(Debug)]
pub struct FileTerminal {
    pub id: TransferId,
    pub direction: TransferDirection,
    pub local: PathBuf,
    pub remote: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub state: TransferState,
    pub error: Option<String>,
}

/// 待扫描目录（frontier 元素）
#[derive(Debug, Clone)]
struct DirToScan {
    local: PathBuf,
    remote: String,
    depth: u32,
}

/// 就绪传输任务（仅存于 ready queue 与 worker，不驻留）
#[derive(Debug)]
struct TransferTask {
    id: TransferId,
    local: PathBuf,
    remote: String,
    size: u64,
}

/// 单目录扫描结果（双车道：files/dirs 各自独立放置）
#[derive(Debug, Default)]
struct Lanes {
    files: VecDeque<(String, u64)>,
    dirs: VecDeque<String>,
}

/// 协调器结果通道消息——每个 op 恰好一条（防死锁不变量 2）
#[derive(Debug)]
enum ScanMsg {
    /// 目录扫描完成（skipped = 该目录内 symlink/特殊文件数）
    Listed {
        parent: DirToScan,
        lanes: Lanes,
        skipped: u64,
    },
    /// 目录扫描/mkdir 失败（该目录放弃，job 继续）
    DirFailed { parent: DirToScan, error: String },
}

/// job 共享状态
struct JobInner {
    id: String,
    direction: TransferDirection,
    roots: Vec<JobRoot>,
    policy: OnExists,
    summary: String,
    state: Mutex<JobState>,
    error: Mutex<Option<String>>,
    failed_entries: Mutex<Vec<FailedEntry>>,
    current: Mutex<HashMap<usize, String>>,
    pause: AtomicBool,
    cancel: AtomicBool,
    scan_done: AtomicBool,
    wake: Notify,
    discovered_files: AtomicU64,
    discovered_bytes: AtomicU64,
    completed_files: AtomicU64,
    failed_files: AtomicU64,
    skipped: AtomicU64,
    bytes_done: AtomicU64,
    /// 已入队未完结任务数（终态判定：scan_done && pending==0 && active==0）
    pending: AtomicU64,
    /// 正在执行的文件数
    active: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl JobInner {
    fn snapshot(&self) -> JobSnapshot {
        JobSnapshot {
            id: self.id.clone(),
            direction: self.direction,
            summary: self.summary.clone(),
            state: *lock(&self.state),
            paused: self.pause.load(Ordering::Relaxed),
            scan_done: self.scan_done.load(Ordering::Relaxed),
            discovered_files: self.discovered_files.load(Ordering::Relaxed),
            discovered_bytes: self.discovered_bytes.load(Ordering::Relaxed),
            completed_files: self.completed_files.load(Ordering::Relaxed),
            failed_files: self.failed_files.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            bytes_done: self.bytes_done.load(Ordering::Relaxed),
            error: lock(&self.error).clone(),
            current: lock(&self.current).values().cloned().collect(),
            failed_entries: lock(&self.failed_entries).clone(),
        }
    }

    fn record_failure(&self, path: String, error: String, cap: usize) {
        self.failed_files.fetch_add(1, Ordering::Relaxed);
        let mut entries = lock(&self.failed_entries);
        if entries.len() < cap {
            entries.push(FailedEntry { path, error });
        }
    }
}

/// 每会话（SftpCtx）一个的目录任务调度器
pub struct DirectoryJobScheduler {
    /// 数据面 subsystem 槽（PR-11/C8）：扫描/传输每次操作经 get() 取当前代际
    data: Arc<SftpSlot>,
    rt: tokio::runtime::Handle,
    /// 执行槽：与 TransferQueue 共享同一信号量（ADR：job 与单文件传输同预算）
    permits: Arc<Semaphore>,
    /// 本地递归扫描配额（app 注入 FsIoLimiter scan 组）
    scan_io: Arc<Semaphore>,
    caps: SchedulerCaps,
    jobs: Mutex<HashMap<String, Arc<JobInner>>>,
    id_seq: AtomicU64,
    on_file_terminal: Mutex<Option<FileTerminalFn>>,
    on_job_terminal: Mutex<Option<JobTerminalFn>>,
}

/// 逐文件终态回调（app 注入 SQLite writer；必须快、不得阻塞）
type FileTerminalFn = Arc<dyn Fn(FileTerminal) + Send + Sync>;
/// job 终态回调（app 注入 audit；同样 fire-and-forget）
type JobTerminalFn = Arc<dyn Fn(JobSnapshot) + Send + Sync>;

impl DirectoryJobScheduler {
    pub fn new(
        data: Arc<SftpSlot>,
        rt: tokio::runtime::Handle,
        permits: Arc<Semaphore>,
        scan_io: Arc<Semaphore>,
    ) -> Arc<Self> {
        Self::with_caps(data, rt, permits, scan_io, SchedulerCaps::default())
    }

    /// 测试用：缩小容量压测边界（生产走 `new`）
    pub fn with_caps(
        data: Arc<SftpSlot>,
        rt: tokio::runtime::Handle,
        permits: Arc<Semaphore>,
        scan_io: Arc<Semaphore>,
        caps: SchedulerCaps,
    ) -> Arc<Self> {
        Arc::new(Self {
            data,
            rt,
            permits,
            scan_io,
            caps,
            jobs: Mutex::new(HashMap::new()),
            id_seq: AtomicU64::new(1),
            on_file_terminal: Mutex::new(None),
            on_job_terminal: Mutex::new(None),
        })
    }

    /// 逐文件终态回调（SQLite history writer；必须快、不得阻塞——try_send 语义）
    pub fn set_file_terminal_callback(&self, cb: Arc<dyn Fn(FileTerminal) + Send + Sync>) {
        *lock(&self.on_file_terminal) = Some(cb);
    }

    /// job 终态回调（audit 等；同样 fire-and-forget）
    pub fn set_job_terminal_callback(&self, cb: Arc<dyn Fn(JobSnapshot) + Send + Sync>) {
        *lock(&self.on_job_terminal) = Some(cb);
    }

    fn new_id(&self, prefix: &str) -> String {
        // 与 TransferQueue::register 同策略：毫秒时间戳 + 进程内序号，跨重启唯一
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!(
            "{prefix}-{millis}-{}",
            self.id_seq.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// 提交目录任务：立即返回 jobId；协调器与 worker 落 bulk-rt
    pub fn submit(self: &Arc<Self>, spec: JobSpec) -> String {
        let id = self.new_id("job");
        let summary = spec
            .roots
            .first()
            .map(|r| format!("{} → {}", r.local.display(), r.remote))
            .unwrap_or_default();
        let job = Arc::new(JobInner {
            id: id.clone(),
            direction: spec.direction,
            summary,
            roots: spec.roots.clone(),
            policy: spec.policy,
            state: Mutex::new(JobState::Scanning),
            error: Mutex::new(None),
            failed_entries: Mutex::new(Vec::new()),
            current: Mutex::new(HashMap::new()),
            pause: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
            scan_done: AtomicBool::new(false),
            wake: Notify::new(),
            discovered_files: AtomicU64::new(0),
            discovered_bytes: AtomicU64::new(0),
            completed_files: AtomicU64::new(0),
            failed_files: AtomicU64::new(0),
            skipped: AtomicU64::new(0),
            bytes_done: AtomicU64::new(0),
            pending: AtomicU64::new(0),
            active: AtomicU64::new(0),
        });
        lock(&self.jobs).insert(id.clone(), job.clone());

        let (ready_tx, ready_rx) = mpsc::channel::<TransferTask>(self.caps.ready);
        // 容量 = 在途上限：每个 op 恰好一条消息，发送永不阻塞（防死锁不变量 2）
        let (page_tx, page_rx) = mpsc::channel::<ScanMsg>(self.caps.in_flight);
        let coord = Coordinator {
            sched: self.clone(),
            job: job.clone(),
            frontier: spec
                .roots
                .into_iter()
                .map(|r| DirToScan {
                    local: r.local,
                    remote: r.remote,
                    depth: 0,
                })
                .collect(),
            held: VecDeque::new(),
            in_flight: Arc::new(AtomicU64::new(0)),
            ready_tx,
            page_tx,
            page_rx,
        };
        self.rt.spawn(coord.run());
        // worker 共享 ready_rx：mpsc Receiver 不可克隆 → Arc+tokio Mutex 分发，
        // select! 保证 cancel/pause 唤醒能取消锁等待与 recv
        let shared_rx = Arc::new(tokio::sync::Mutex::new(ready_rx));
        for idx in 0..WORKERS {
            let w = Worker {
                sched: self.clone(),
                job: job.clone(),
                idx,
                rx: shared_rx.clone(),
            };
            self.rt.spawn(w.run());
        }
        id
    }

    pub fn list(&self) -> Vec<JobSnapshot> {
        lock(&self.jobs).values().map(|j| j.snapshot()).collect()
    }

    /// registry 内 job 数（perf_json 用，不取快照）
    pub fn job_count(&self) -> usize {
        lock(&self.jobs).len()
    }

    fn with<R>(&self, id: &str, f: impl FnOnce(&Arc<JobInner>) -> R) -> Result<R, SftpError> {
        let job = lock(&self.jobs)
            .get(id)
            .cloned()
            .ok_or_else(|| SftpError::RemotePath {
                path: id.to_string(),
                reason: "任务不存在".into(),
            })?;
        Ok(f(&job))
    }

    pub fn pause(&self, id: &str) -> Result<(), SftpError> {
        self.with(id, |j| j.pause.store(true, Ordering::Relaxed))
    }

    pub fn resume(&self, id: &str) -> Result<(), SftpError> {
        self.with(id, |j| {
            j.pause.store(false, Ordering::Relaxed);
            j.wake.notify_waiters();
        })
    }

    /// 取消：停止发现新任务、丢弃未开始任务；在途文件 chunk 边界中断
    pub fn cancel(&self, id: &str) -> Result<(), SftpError> {
        let job = self.with(id, |j| j.clone())?;
        {
            let mut state = lock(&job.state);
            if state.is_terminal() {
                return Err(SftpError::RemotePath {
                    path: id.to_string(),
                    reason: format!("任务已终态（{}）", state.as_str()),
                });
            }
            *state = JobState::Canceled;
        }
        job.cancel.store(true, Ordering::Relaxed);
        job.pause.store(false, Ordering::Relaxed);
        job.wake.notify_waiters();
        self.emit_job_terminal(&job);
        Ok(())
    }

    /// 重试：终态 job 以 Resume 策略重扫重跑（已完成文件偏移即全长、秒级短路），
    /// 返回新 jobId；不依赖失败清单完整性（ADR 决策 3）
    pub fn retry(self: &Arc<Self>, id: &str) -> Result<String, SftpError> {
        let (direction, roots) = self.with(id, |j| {
            if !lock(&j.state).is_terminal() {
                return Err(SftpError::RemotePath {
                    path: id.to_string(),
                    reason: "仅终态任务可重试".into(),
                });
            }
            Ok((j.direction, j.roots.clone()))
        })??;
        Ok(self.submit(JobSpec {
            direction,
            roots,
            policy: OnExists::Resume,
        }))
    }

    /// 移除：仅终态可移除
    pub fn remove(&self, id: &str) -> Result<(), SftpError> {
        let mut jobs = lock(&self.jobs);
        let job = jobs.get(id).ok_or_else(|| SftpError::RemotePath {
            path: id.to_string(),
            reason: "任务不存在".into(),
        })?;
        let state = *lock(&job.state);
        if !state.is_terminal() {
            return Err(SftpError::RemotePath {
                path: id.to_string(),
                reason: format!("仅终态任务可移除（当前: {}）", state.as_str()),
            });
        }
        jobs.remove(id);
        Ok(())
    }

    /// 暂停全部进行中 job（配合 transfer_pause_all）
    pub fn pause_all(&self) {
        for j in lock(&self.jobs).values() {
            if !lock(&j.state).is_terminal() {
                j.pause.store(true, Ordering::Relaxed);
            }
        }
    }

    /// 恢复全部已暂停 job
    pub fn resume_all(&self) {
        for j in lock(&self.jobs).values() {
            if j.pause.swap(false, Ordering::Relaxed) {
                j.wake.notify_waiters();
            }
        }
    }

    fn emit_file_terminal(&self, rec: FileTerminal) {
        let cb = lock(&self.on_file_terminal).clone();
        if let Some(cb) = cb {
            cb(rec);
        }
    }

    fn emit_job_terminal(&self, job: &Arc<JobInner>) {
        let cb = lock(&self.on_job_terminal).clone();
        if let Some(cb) = cb {
            cb(job.snapshot());
        }
    }

    /// 终态检查：scan_done && pending==0 && active==0 → Completed（或致命 error → Failed）。
    /// 协调器扫描结束、每个任务完结、每个 worker 退出时各调一次；幂等。
    fn check_terminal(&self, job: &Arc<JobInner>) {
        if !(job.scan_done.load(Ordering::Relaxed)
            && job.pending.load(Ordering::Relaxed) == 0
            && job.active.load(Ordering::Relaxed) == 0)
        {
            return;
        }
        let mut state = lock(&job.state);
        if state.is_terminal() {
            return;
        }
        *state = if lock(&job.error).is_some() {
            JobState::Failed
        } else {
            JobState::Completed
        };
        drop(state);
        self.emit_job_terminal(job);
    }
}

/// 更新 job 进行相（Scanning → Transferring → Finalizing）
fn refresh_phase(job: &JobInner) {
    let mut state = lock(&job.state);
    if state.is_terminal() {
        return;
    }
    let scan_done = job.scan_done.load(Ordering::Relaxed);
    let idle = job.pending.load(Ordering::Relaxed) == 0 && job.active.load(Ordering::Relaxed) == 0;
    *state = match (scan_done, idle) {
        (false, _) => JobState::Scanning,
        (true, false) => JobState::Transferring,
        (true, true) => JobState::Finalizing,
    };
}

/// 冲突解析（与 app 层 resolve_remote_target/resolve_local_target 同语义，
/// 移入 worker 逐文件执行；Ok(None) = skip）
async fn resolve_remote(
    data: &Arc<SftpSlot>,
    target: &str,
    policy: OnExists,
) -> Result<Option<(String, OnExists)>, SftpError> {
    let sftp = data.get().await?;
    if sftp.stat(target).await.is_err() {
        return Ok(Some((target.to_string(), policy.runtime())));
    }
    match policy {
        OnExists::Resume | OnExists::Overwrite => Ok(Some((target.to_string(), policy))),
        OnExists::Skip => Ok(None),
        OnExists::Rename => {
            for n in 1..1000 {
                let cand = crate::rename_candidate(target, n);
                if sftp.stat(&cand).await.is_err() {
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

fn resolve_local(
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

/// ScanCoordinator：frontier/held/ready 的唯一协调方（C2）
struct Coordinator {
    sched: Arc<DirectoryJobScheduler>,
    job: Arc<JobInner>,
    frontier: VecDeque<DirToScan>,
    /// 已收未放置完的结果（容量 ≤ in_flight：放不下的结果暂留通道，op 不因此阻塞）
    held: VecDeque<(DirToScan, Lanes)>,
    /// 在途 op 数（防死锁不变量 3：op 发送完终态消息即归还，与协调器接收解耦）
    in_flight: Arc<AtomicU64>,
    ready_tx: mpsc::Sender<TransferTask>,
    page_tx: mpsc::Sender<ScanMsg>,
    page_rx: mpsc::Receiver<ScanMsg>,
}

impl Coordinator {
    async fn run(mut self) {
        loop {
            if self.job.cancel.load(Ordering::Relaxed) {
                break;
            }
            // 1) 尽力放置 held（双车道互不阻塞；逐条遍历全部 held 结果）
            self.place_held();
            // 2) 发起新 op（pop 即释放 frontier 槽位；上传 op 内自带 mkdir 关）
            while self.in_flight.load(Ordering::Relaxed) < self.sched.caps.in_flight as u64
                && !self.frontier.is_empty()
            {
                let dir = match self.frontier.pop_front() {
                    Some(d) => d,
                    None => break,
                };
                self.spawn_scan(dir);
            }
            // 3) 扫描完结判定（page_rx.is_empty 见不变量 3：槽位先还、消息后到的窗口）
            if self.frontier.is_empty()
                && self.held.is_empty()
                && self.in_flight.load(Ordering::Relaxed) == 0
                && self.page_rx.is_empty()
            {
                break;
            }
            // 4) 等事件：op 终态消息（held 满则暂留通道）/ 控制唤醒 / 保底轮询
            let held_full = self.held.len() >= self.sched.caps.in_flight;
            tokio::select! {
                msg = self.page_rx.recv(), if !held_full => {
                    match msg {
                        Some(ScanMsg::Listed { parent, lanes, skipped }) => {
                            if skipped > 0 {
                                self.job.skipped.fetch_add(skipped, Ordering::Relaxed);
                            }
                            for (_, size) in &lanes.files {
                                self.job.discovered_files.fetch_add(1, Ordering::Relaxed);
                                self.job.discovered_bytes.fetch_add(*size, Ordering::Relaxed);
                            }
                            if !lanes.files.is_empty() || !lanes.dirs.is_empty() {
                                self.held.push_back((parent, lanes));
                            }
                        }
                        Some(ScanMsg::DirFailed { parent, error }) => {
                            let path = match self.job.direction {
                                TransferDirection::Upload => parent.local.display().to_string(),
                                TransferDirection::Download => parent.remote.clone(),
                            };
                            self.job.record_failure(
                                path,
                                error,
                                self.sched.caps.failed_entries,
                            );
                        }
                        // 协调器自持 page_tx，None 不可达；防御性退出
                        None => break,
                    }
                }
                _ = self.job.wake.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(REPOLLS_MS)) => {}
            }
        }
        // 扫描结束：关闭 ready（worker 排空后退出），标记 scan_done 并做终态检查
        drop(self.ready_tx);
        self.job.scan_done.store(true, Ordering::Relaxed);
        refresh_phase(&self.job);
        self.sched.check_terminal(&self.job);
    }

    fn place_held(&mut self) {
        let mut i = 0;
        while i < self.held.len() {
            let (parent, lanes) = &mut self.held[i];
            // dirs 车道：frontier 满则该结果 dirs 等待（不堵其他结果的 files 车道）
            while let Some(name) = lanes.dirs.front() {
                if self.frontier.len() >= self.sched.caps.frontier {
                    break;
                }
                let name = name.clone();
                if parent.depth + 1 > self.sched.caps.max_depth {
                    lanes.dirs.pop_front();
                    self.job.skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                let child = DirToScan {
                    local: parent.local.join(&name),
                    remote: format!("{}/{name}", parent.remote),
                    depth: parent.depth + 1,
                };
                if child.remote.len() > MAX_PATH || child.local.as_os_str().len() > MAX_PATH {
                    lanes.dirs.pop_front();
                    self.job.skipped.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                lanes.dirs.pop_front();
                self.frontier.push_back(child);
            }
            // files 车道：暂停/取消或 ready 满则等待
            while let Some((name, size)) = lanes.files.front() {
                if self.job.pause.load(Ordering::Relaxed) || self.job.cancel.load(Ordering::Relaxed)
                {
                    break;
                }
                let task = TransferTask {
                    id: self.sched.new_id("tr"),
                    local: parent.local.join(name),
                    remote: format!("{}/{name}", parent.remote),
                    size: *size,
                };
                match self.ready_tx.try_send(task) {
                    Ok(()) => {
                        lanes.files.pop_front();
                        self.job.pending.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(_) => break,
                }
            }
            if lanes.files.is_empty() && lanes.dirs.is_empty() {
                self.held.remove(i);
            } else {
                i += 1;
            }
        }
    }

    fn spawn_scan(&self, dir: DirToScan) {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        let in_flight = self.in_flight.clone();
        let tx = self.page_tx.clone();
        match self.job.direction {
            TransferDirection::Download => {
                let data = self.sched.data.clone();
                self.sched.rt.spawn(async move {
                    let msg = match data.get().await {
                        Err(e) => ScanMsg::DirFailed {
                            parent: dir,
                            error: e.to_string(),
                        },
                        Ok(sftp) => match sftp.list(&dir.remote).await {
                            Ok(entries) => {
                                let mut lanes = Lanes::default();
                                let mut skipped = 0u64;
                                for e in entries {
                                    match e.kind {
                                        EntryKind::File => lanes.files.push_back((e.name, e.size)),
                                        EntryKind::Dir => lanes.dirs.push_back(e.name),
                                        // symlink/特殊文件不跟随（ADR 决策 7：防环）
                                        EntryKind::Symlink | EntryKind::Other => skipped += 1,
                                    }
                                }
                                ScanMsg::Listed {
                                    parent: dir,
                                    lanes,
                                    skipped,
                                }
                            }
                            Err(e) => {
                                data.record_error();
                                ScanMsg::DirFailed {
                                    parent: dir,
                                    error: e.to_string(),
                                }
                            }
                        },
                    };
                    let _ = tx.send(msg).await;
                    in_flight.fetch_sub(1, Ordering::Relaxed);
                });
            }
            TransferDirection::Upload => {
                // mkdir 关：父目录远端 mkdir 先于其文件入 ready（幂等忽略已存在）
                let data = self.sched.data.clone();
                let sem = self.sched.scan_io.clone();
                self.sched.rt.spawn(async move {
                    let mkdir_err = match data.get().await {
                        Err(e) => Some(e.to_string()),
                        Ok(sftp) => match sftp.mkdir(&dir.remote).await {
                            Ok(()) => None,
                            Err(e) if e.to_string().contains("Failure") => None,
                            Err(e) => {
                                data.record_error();
                                Some(e.to_string())
                            }
                        },
                    };
                    if let Some(e) = mkdir_err {
                        let _ = tx
                            .send(ScanMsg::DirFailed {
                                parent: dir,
                                error: format!("远端建目录失败: {e}"),
                            })
                            .await;
                        in_flight.fetch_sub(1, Ordering::Relaxed);
                        return;
                    }
                    // FsIoLimiter scan 组配额：慢盘上递归扫描严格限并发（PR-3）
                    let permit = sem.acquire_owned().await;
                    match permit {
                        Ok(p) => {
                            let dir2 = dir.clone();
                            let r = tokio::task::spawn_blocking(move || {
                                let _permit = p;
                                scan_local_dir(dir2)
                            })
                            .await;
                            let msg = match r {
                                Ok(m) => m,
                                Err(e) => ScanMsg::DirFailed {
                                    parent: dir,
                                    error: format!("本地扫描任务失败: {e}"),
                                },
                            };
                            let _ = tx.send(msg).await;
                        }
                        Err(_) => {
                            let _ = tx
                                .send(ScanMsg::DirFailed {
                                    parent: dir,
                                    error: "扫描限流器已关闭".into(),
                                })
                                .await;
                        }
                    }
                    in_flight.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
    }
}

/// 本地目录扫描（spawn_blocking 内；file_type 不跟随 symlink，等价 lstat）
fn scan_local_dir(parent: DirToScan) -> ScanMsg {
    let rd = match std::fs::read_dir(&parent.local) {
        Ok(r) => r,
        Err(e) => {
            return ScanMsg::DirFailed {
                parent,
                error: e.to_string(),
            };
        }
    };
    let mut lanes = Lanes::default();
    let mut skipped = 0u64;
    for e in rd {
        match e {
            Ok(e) => {
                let name = e.file_name().to_string_lossy().to_string();
                match e.file_type() {
                    Ok(ft) if ft.is_dir() => lanes.dirs.push_back(name),
                    Ok(ft) if ft.is_file() => {
                        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                        lanes.files.push_back((name, size));
                    }
                    // symlink/特殊文件：不跟随（防环）
                    _ => skipped += 1,
                }
            }
            Err(err) => {
                return ScanMsg::DirFailed {
                    parent,
                    error: err.to_string(),
                };
            }
        }
    }
    ScanMsg::Listed {
        parent,
        lanes,
        skipped,
    }
}

/// 执行 worker：ready → 冲突解析 → 共享执行槽 → download/upload_once（断点续传）
struct Worker {
    sched: Arc<DirectoryJobScheduler>,
    job: Arc<JobInner>,
    idx: usize,
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<TransferTask>>>,
}

impl Worker {
    async fn run(self) {
        loop {
            if self.job.cancel.load(Ordering::Relaxed) {
                break;
            }
            self.wait_unpaused().await;
            // 共享 Receiver：select! 使 wake 能取消锁等待与 recv（cancel/pause 即时生效）
            let rx = self.rx.clone();
            let task = tokio::select! {
                t = async { rx.lock().await.recv().await } => t,
                _ = self.job.wake.notified() => continue,
            };
            let Some(task) = task else { break }; // ready 关闭且排空 → 退出
            if self.job.cancel.load(Ordering::Relaxed) {
                break;
            }
            self.run_task(task).await;
            self.job.pending.fetch_sub(1, Ordering::Relaxed);
            refresh_phase(&self.job);
            self.sched.check_terminal(&self.job);
        }
        // worker 退出收口：与协调器 scan_done/其他 worker 的竞争由幂等 check 吸收
        self.sched.check_terminal(&self.job);
    }

    async fn wait_unpaused(&self) {
        while self.job.pause.load(Ordering::Relaxed) && !self.job.cancel.load(Ordering::Relaxed) {
            self.job.wake.notified().await;
        }
    }

    async fn run_task(&self, task: TransferTask) {
        // 冲突解析（逐文件；skip 计 skipped）
        let resolved = match self.job.direction {
            TransferDirection::Upload => {
                resolve_remote(&self.sched.data, &task.remote, self.job.policy)
                    .await
                    .map(|o| o.map(|(remote, mode)| (task.local.clone(), remote, mode)))
            }
            TransferDirection::Download => resolve_local(&task.local, self.job.policy)
                .map(|o| o.map(|(local, mode)| (local, task.remote.clone(), mode))),
        };
        let (local, remote, mode) = match resolved {
            Ok(Some(v)) => v,
            Ok(None) => {
                self.job.skipped.fetch_add(1, Ordering::Relaxed);
                return;
            }
            Err(e) => {
                self.job.record_failure(
                    display_path(&self.job, &task),
                    e.to_string(),
                    self.sched.caps.failed_entries,
                );
                return;
            }
        };

        let mut attempt = 0u32;
        loop {
            self.wait_unpaused().await;
            if self.job.cancel.load(Ordering::Relaxed) {
                self.report(&task, 0, TransferState::Canceled, None);
                return;
            }
            let permit = match self.sched.permits.acquire().await {
                Ok(p) => p,
                Err(_) => return, // 信号量关闭 = 调度器销毁
            };
            if self.job.cancel.load(Ordering::Relaxed) || self.job.pause.load(Ordering::Relaxed) {
                drop(permit);
                continue;
            }
            let info = TransferInfo {
                id: task.id.clone(),
                direction: self.job.direction,
                local: local.clone(),
                remote: remote.clone(),
                state: TransferState::Running,
                bytes_done: 0,
                bytes_total: task.size,
                on_exists: mode,
                retries: attempt,
                error: None,
            };
            let t = TransferInner::new_transient(info);
            // job 级 pause/cancel → transient 位传播（chunk 边界中断）
            let watch = {
                let job = self.job.clone();
                let t = t.clone();
                tokio::spawn(async move {
                    loop {
                        job.wake.notified().await;
                        t.signal(
                            job.pause.load(Ordering::Relaxed),
                            job.cancel.load(Ordering::Relaxed),
                        );
                        if job.cancel.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                })
            };
            let display = match self.job.direction {
                TransferDirection::Upload => local.display().to_string(),
                TransferDirection::Download => remote.clone(),
            };
            lock(&self.job.current).insert(self.idx, display);
            self.job.active.fetch_add(1, Ordering::Relaxed);
            let result = match self.sched.data.get().await {
                Ok(client) => match self.job.direction {
                    TransferDirection::Download => download_once(client, t.clone()).await,
                    TransferDirection::Upload => upload_once(client, t.clone()).await,
                },
                Err(e) => Err(e),
            };
            self.job.active.fetch_sub(1, Ordering::Relaxed);
            lock(&self.job.current).remove(&self.idx);
            watch.abort();
            drop(permit);
            let bytes = t.final_bytes();

            match result {
                Ok(()) => {
                    self.job.completed_files.fetch_add(1, Ordering::Relaxed);
                    self.job.bytes_done.fetch_add(bytes, Ordering::Relaxed);
                    self.report(&task, bytes, TransferState::Done, None);
                    return;
                }
                Err(e) => {
                    if self.job.cancel.load(Ordering::Relaxed) {
                        self.job.bytes_done.fetch_add(bytes, Ordering::Relaxed);
                        self.report(&task, bytes, TransferState::Canceled, Some(e.to_string()));
                        return;
                    }
                    if self.job.pause.load(Ordering::Relaxed) {
                        continue; // 暂停中断不算失败：等恢复后从断点重跑（不占 permit）
                    }
                    // 真实失败：标 data 代际可疑，下次取用先探活、死了则单飞重建（C8）
                    self.sched.data.record_error();
                    attempt += 1;
                    if attempt > MAX_FILE_RETRIES {
                        self.job.bytes_done.fetch_add(bytes, Ordering::Relaxed);
                        self.job.record_failure(
                            display_path(&self.job, &task),
                            e.to_string(),
                            self.sched.caps.failed_entries,
                        );
                        self.report(&task, bytes, TransferState::Failed, Some(e.to_string()));
                        return;
                    }
                    // 重试退避 1s（不持 permit，对齐 run_transfer）
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    fn report(&self, task: &TransferTask, bytes: u64, state: TransferState, error: Option<String>) {
        self.sched.emit_file_terminal(FileTerminal {
            id: task.id.clone(),
            direction: self.job.direction,
            local: task.local.clone(),
            remote: task.remote.clone(),
            bytes_done: bytes,
            bytes_total: task.size,
            state,
            error,
        });
    }
}

fn display_path(job: &JobInner, task: &TransferTask) -> String {
    match job.direction {
        TransferDirection::Upload => task.local.display().to_string(),
        TransferDirection::Download => task.remote.clone(),
    }
}
