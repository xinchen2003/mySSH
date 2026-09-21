//! 队列化传输：并发可控、断点续传、失败重试、暂停/取消、进度回调。
//!
//! 设计要点：
//! - 并发上限用 Budget 账本（PR-17；默认 3），超过排队
//! - 每个传输 = 独立 tokio 任务，分块 256KB（对齐 russh-sftp max_packet_len）
//! - 续传：下载看本地已有长度；上传先 stat 远端长度，从断点继续
//! - 重试：失败自动重试（默认 2 次），每次从当前断点继续
//! - 终态条目支持手动重试（retry）/移除（remove）/批量清理（clear_where）
//! - russh-sftp 写为 fire-and-forget，完成前必须 shutdown 排空写确认
//!   （否则最后若干包可能未落地——集成测试踩过）
//! - 进度经回调外发（app 层接 Channel 推送 UI）；速率由调用方按采样算

use crate::{SftpClient, SftpError, SftpSlot, TransferDirection};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
pub type TransferId = String;

/// 目标已存在时的处理策略。
/// serde 默认 resume：历史数据/旧前端缺字段反序列化时保持既有续传行为。
/// skip/rename 由命令层在入队前解析（skip 不入队、rename 换成新名），
/// 运行期只剩 resume/overwrite 两种语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnExists {
    /// 断点续传（默认）：目标已有长度即偏移；目标比源长 = 内容不符，归零重传
    #[default]
    Resume,
    /// 覆盖：偏移 0 + 截断重传
    Overwrite,
    /// 跳过：冲突文件不入队（命令层计入 skipped）
    Skip,
    /// 自动改名：命令层找 name-N.ext 空名后按 Resume 入队
    Rename,
}

impl OnExists {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resume => "resume",
            Self::Overwrite => "overwrite",
            Self::Skip => "skip",
            Self::Rename => "rename",
        }
    }
    /// 命令层解析后的运行期语义：skip 不入队、rename 已改名，二者都不会到达运行期
    pub fn runtime(self) -> Self {
        match self {
            Self::Overwrite => Self::Overwrite,
            _ => Self::Resume,
        }
    }
}

/// 续传偏移决策（护栏：目标比源还长 = 内容不符的脏续写，偏移归零截断重传）。
/// 上传：target=远端现有长度，source=本地大小；下载：target=本地已有长度，source=远端大小。
fn resume_offset(target_len: u64, source_len: u64, mode: OnExists) -> u64 {
    match mode {
        OnExists::Overwrite => 0,
        _ => {
            if target_len > source_len {
                0
            } else {
                target_len
            }
        }
    }
}

/// 改名候选：`dir/name.ext` + 2 → `dir/name-2.ext`。
/// 无扩展名 → `name-2`；隐藏文件（.env）不拆扩展名 → `.env-2`。
/// 远端（/）与本地（\）分隔符都识别。
pub fn rename_candidate(path: &str, n: u32) -> String {
    let (dir, name) = match path.rfind(['/', '\\']) {
        Some(i) => (&path[..=i], &path[i + 1..]),
        None => ("", path),
    };
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    format!("{dir}{stem}-{n}{ext}")
}

/// 传输状态机
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Canceled,
}

impl TransferState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
    /// 终态：生命周期结束（可移除/清理/重试的判定基准）
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Canceled)
    }
}

/// 单个传输的可观测快照
#[derive(Debug, Clone)]
pub struct TransferInfo {
    pub id: TransferId,
    pub direction: TransferDirection,
    pub local: PathBuf,
    pub remote: String,
    pub state: TransferState,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// 冲突策略（入队时解析后的运行期语义：resume/overwrite）
    pub on_exists: OnExists,
    /// 已自动重试次数
    pub retries: u32,
    pub error: Option<String>,
}

/// 单文件传输的共享状态。DirectoryJob worker 为在途文件建 transient 实例
///（不入 transfers registry、随文件结束即弃），复用 pause/cancel/断点机制。
pub(crate) struct TransferInner {
    info: Mutex<TransferInfo>,
    bytes_done: AtomicU64,
    pause: AtomicBool,
    cancel: AtomicBool,
}

impl TransferInner {
    /// job worker 用：构造不入 registry 的 transient 实例
    pub(crate) fn new_transient(info: TransferInfo) -> Arc<Self> {
        Arc::new(Self {
            info: Mutex::new(info),
            bytes_done: AtomicU64::new(0),
            pause: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
        })
    }

    /// job 取消/暂停传播：置位后 download_once/upload_once 在 chunk 边界中断
    pub(crate) fn signal(&self, pause: bool, cancel: bool) {
        self.pause.store(pause, Ordering::Relaxed);
        self.cancel.store(cancel, Ordering::Relaxed);
    }

    /// 结束时的实际字节量（含续传偏移；job 计数用）
    pub(crate) fn final_bytes(&self) -> u64 {
        self.bytes_done.load(Ordering::Relaxed)
    }
}

/// 进度回调（每个分块落地后调用；实现必须快，不得阻塞）
pub type ProgressFn = Arc<dyn Fn(TransferInfo) + Send + Sync>;

pub struct TransferQueue {
    /// 数据面 subsystem 槽（PR-11/C8）：每次尝试经 get() 取当前代际 client，
    /// data 代际重建后重试自动落到新 client；失败经 record_error 上报可疑
    data: Arc<SftpSlot>,
    rt: tokio::runtime::Handle,
    /// 执行槽（DirectoryJob worker 与单文件传输共享同一并发预算，ADR 0001；
    /// PR-17 账本化：active/rejected 指标直接可读）
    pub(crate) permits: Arc<core_policy::Budget>,
    transfers: Mutex<HashMap<TransferId, Arc<TransferInner>>>,
    id_seq: AtomicU64,
    max_retries: u32,
    on_progress: Mutex<Option<ProgressFn>>,
}

const CHUNK: usize = 256 * 1024;
/// 下载 read-ahead 流水线深度（PR-16 P4-1）：在途窗口 = DEPTH × CHUNK = 2MiB。
/// 先取保守值；P4-2 参数矩阵（effective_packet × in_flight × RTT）出数据后再调
const READ_AHEAD_DEPTH: usize = 8;

/// 锁中毒自愈（panic 现场已恢复，数据本身无损坏语义）
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl TransferQueue {
    pub fn new(data: Arc<SftpSlot>, max_concurrent: usize, rt: tokio::runtime::Handle) -> Self {
        Self {
            data,
            rt,
            permits: core_policy::Budget::new("sftp.exec", max_concurrent.max(1)),
            transfers: Mutex::new(HashMap::new()),
            id_seq: AtomicU64::new(1),
            max_retries: 2,
            on_progress: Mutex::new(None),
        }
    }

    /// 执行槽句柄（DirectoryJobScheduler 与单文件传输共享同一并发预算，ADR 0001）
    pub fn permits_handle(&self) -> Arc<core_policy::Budget> {
        self.permits.clone()
    }

    /// 执行槽预算快照（PR-17 perf_json governor 节数据源）
    pub fn exec_budget_snapshot(&self) -> core_policy::BudgetSnapshot {
        self.permits.snapshot()
    }

    pub fn set_progress_callback(&self, cb: ProgressFn) {
        *lock(&self.on_progress) = Some(cb);
    }

    fn snapshot(t: &Arc<TransferInner>) -> TransferInfo {
        let mut info = lock(&t.info).clone();
        info.bytes_done = t.bytes_done.load(Ordering::Relaxed);
        info
    }

    fn emit(&self, t: &Arc<TransferInner>) {
        let cb = lock(&self.on_progress).clone();
        if let Some(cb) = cb {
            cb(Self::snapshot(t));
        }
    }

    fn register(
        &self,
        direction: TransferDirection,
        local: PathBuf,
        remote: String,
        bytes_total: u64,
        on_exists: OnExists,
    ) -> (TransferId, Arc<TransferInner>) {
        // ID 跨进程唯一（毫秒时间戳 + 进程内序号）：终态会落 transfers 表并按 id upsert，
        // 纯进程内序号在重启后从 tr-1 重排，会覆盖掉上次运行留下的历史记录。
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let id = format!(
            "tr-{}-{}",
            millis,
            self.id_seq.fetch_add(1, Ordering::Relaxed)
        );
        let inner = Arc::new(TransferInner {
            info: Mutex::new(TransferInfo {
                id: id.clone(),
                direction,
                local,
                remote,
                state: TransferState::Queued,
                bytes_done: 0,
                bytes_total,
                on_exists,
                retries: 0,
                error: None,
            }),
            bytes_done: AtomicU64::new(0),
            pause: AtomicBool::new(false),
            cancel: AtomicBool::new(false),
        });
        lock(&self.transfers).insert(id.clone(), inner.clone());
        (id, inner)
    }

    /// 入队下载（remote -> local）。目录递归由 app 层展开为逐文件入队。
    pub async fn enqueue_download(
        self: &Arc<Self>,
        remote: String,
        local: PathBuf,
        bytes_total: u64,
        on_exists: OnExists,
    ) -> TransferId {
        self.enqueue(
            TransferDirection::Download,
            local,
            remote,
            bytes_total,
            on_exists,
        )
        .await
    }

    /// 入队上传（local -> remote）
    pub async fn enqueue_upload(
        self: &Arc<Self>,
        local: PathBuf,
        remote: String,
        bytes_total: u64,
        on_exists: OnExists,
    ) -> TransferId {
        self.enqueue(
            TransferDirection::Upload,
            local,
            remote,
            bytes_total,
            on_exists,
        )
        .await
    }

    async fn enqueue(
        self: &Arc<Self>,
        direction: TransferDirection,
        local: PathBuf,
        remote: String,
        bytes_total: u64,
        on_exists: OnExists,
    ) -> TransferId {
        let (id, inner) = self.register(direction, local, remote, bytes_total, on_exists);
        let q = self.clone();
        self.rt.spawn(async move {
            q.run_transfer(inner).await;
        });
        id
    }

    /// 通用执行器：非暂停等待 → 并发闸 → 断点重试环。
    ///
    /// P1-8 不变量：暂停自旋与重试退避一律不持有 permit（先 acquire 后自旋会让
    /// 3 个暂停任务占满并发闸、饿死正常任务）。传输中途暂停在 chunk 边界经
    /// Interrupted 中断、释放 permit 后回非暂停等待；resume 后重新竞争 permit，
    /// 断点由 download_once/upload_once 的续传逻辑从已确认偏移恢复
    /// （上传=远端 stat 的 ACK 连续前缀，下载=本地文件已写长度）。
    async fn run_transfer(&self, t: Arc<TransferInner>) {
        let direction = lock(&t.info).direction;
        loop {
            if t.cancel.load(Ordering::Relaxed) {
                lock(&t.info).state = TransferState::Canceled;
                self.emit(&t);
                return;
            }
            // 非暂停等待（200ms 粒度；不持 permit，不占并发额度）
            if t.pause.load(Ordering::Relaxed) {
                lock(&t.info).state = TransferState::Paused;
                self.emit(&t);
                while t.pause.load(Ordering::Relaxed) && !t.cancel.load(Ordering::Relaxed) {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                continue; // 回顶部：cancel 分支或重新竞争 permit
            }
            let permit = self.permits.acquire().await;
            // acquire 等待期间可能被暂停/取消：二次确认，避免持 permit 进传输
            if t.cancel.load(Ordering::Relaxed) || t.pause.load(Ordering::Relaxed) {
                drop(permit);
                continue; // 交由顶部 cancel/pause 分支
            }
            lock(&t.info).state = TransferState::Running;
            self.emit(&t);
            let result = match self.data.get().await {
                Ok(client) => match direction {
                    TransferDirection::Download => download_once(client, t.clone()).await,
                    TransferDirection::Upload => upload_once(client, t.clone()).await,
                },
                Err(e) => Err(e),
            };
            // 执行单元结束（完成/中断/失败）：立即释放 permit，后续去向均不持有它
            drop(permit);
            match result {
                Ok(()) => {
                    lock(&t.info).state = TransferState::Done;
                    self.emit(&t);
                    return;
                }
                Err(e) => {
                    if t.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    // 暂停引发的断点中断不算失败重试：回顶部非暂停等待（无 permit）
                    if t.pause.load(Ordering::Relaxed) {
                        continue;
                    }
                    // 真实失败：标 data 代际可疑，下次取用先探活、死了则单飞重建（C8）
                    self.data.record_error();
                    let over = {
                        let mut info = lock(&t.info);
                        info.retries += 1;
                        info.error = Some(e.to_string());
                        info.retries > self.max_retries
                    };
                    if over {
                        lock(&t.info).state = TransferState::Failed;
                        self.emit(&t);
                        return;
                    }
                    self.emit(&t);
                    // 重试退避：1s、2s（不持 permit）
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    pub fn list(&self) -> Vec<TransferInfo> {
        lock(&self.transfers).values().map(Self::snapshot).collect()
    }

    pub fn get(&self, id: &str) -> Option<TransferInfo> {
        lock(&self.transfers).get(id).map(Self::snapshot)
    }

    fn with<R>(&self, id: &str, f: impl FnOnce(&TransferInner) -> R) -> Result<R, SftpError> {
        let t = lock(&self.transfers)
            .get(id)
            .cloned()
            .ok_or_else(|| SftpError::RemotePath {
                path: id.to_string(),
                reason: "传输不存在".into(),
            })?;
        Ok(f(&t))
    }

    pub fn pause(&self, id: &str) -> Result<(), SftpError> {
        self.with(id, |t| t.pause.store(true, Ordering::Relaxed))
    }

    pub fn resume(&self, id: &str) -> Result<(), SftpError> {
        self.with(id, |t| {
            t.pause.store(false, Ordering::Relaxed);
            lock(&t.info).state = TransferState::Running;
        })
    }

    pub fn cancel(&self, id: &str) -> Result<(), SftpError> {
        self.with(id, |t| {
            t.cancel.store(true, Ordering::Relaxed);
            t.pause.store(false, Ordering::Relaxed);
        })
    }
    /// 重试：仅 Failed/Canceled 可重跑。重置为 Queued 后 respawn run_transfer，
    /// 断点由 download_once/upload_once 的既有续传逻辑自动沿用（无需显式传断点）。
    pub fn retry(self: &Arc<Self>, id: &str) -> Result<(), SftpError> {
        let inner =
            lock(&self.transfers)
                .get(id)
                .cloned()
                .ok_or_else(|| SftpError::RemotePath {
                    path: id.to_string(),
                    reason: "传输不存在".into(),
                })?;
        {
            let mut info = lock(&inner.info);
            if !matches!(info.state, TransferState::Failed | TransferState::Canceled) {
                return Err(SftpError::RemotePath {
                    path: id.to_string(),
                    reason: format!("仅失败/已取消的传输可重试（当前: {}）", info.state.as_str()),
                });
            }
            info.state = TransferState::Queued;
            info.retries = 0;
            info.error = None;
        }
        inner.pause.store(false, Ordering::Relaxed);
        inner.cancel.store(false, Ordering::Relaxed);
        // 进度清零重来（断点仍在文件系统侧，开跑后由续传逻辑回填）
        inner.bytes_done.store(0, Ordering::Relaxed);
        self.emit(&inner);
        let q = self.clone();
        self.rt.spawn(async move {
            q.run_transfer(inner).await;
        });
        Ok(())
    }

    /// 移除条目：仅终态可移除，进行中的一律拒绝
    pub fn remove(&self, id: &str) -> Result<(), SftpError> {
        let mut map = lock(&self.transfers);
        let t = map.get(id).ok_or_else(|| SftpError::RemotePath {
            path: id.to_string(),
            reason: "传输不存在".into(),
        })?;
        let state = lock(&t.info).state;
        if !state.is_terminal() {
            return Err(SftpError::RemotePath {
                path: id.to_string(),
                reason: format!("仅终态传输可移除（当前: {}）", state.as_str()),
            });
        }
        map.remove(id);
        Ok(())
    }

    /// 批量移除满足条件的终态条目（非终态一律跳过），返回移除数
    pub fn clear_where(&self, pred: impl Fn(TransferState) -> bool) -> u32 {
        let mut map = lock(&self.transfers);
        let before = map.len();
        map.retain(|_, t| {
            let s = lock(&t.info).state;
            !(s.is_terminal() && pred(s))
        });
        (before - map.len()) as u32
    }

    /// 暂停全部 Queued/Running（终态与已暂停不受影响）
    pub fn pause_all(&self) {
        for t in lock(&self.transfers).values() {
            let s = lock(&t.info).state;
            if matches!(s, TransferState::Queued | TransferState::Running) {
                t.pause.store(true, Ordering::Relaxed);
            }
        }
    }

    /// 恢复全部已暂停（语义同 resume：清暂停位并置 Running，Queued 项开跑后自校正）
    pub fn resume_all(&self) {
        for t in lock(&self.transfers).values() {
            if t.pause.swap(false, Ordering::Relaxed) {
                lock(&t.info).state = TransferState::Running;
            }
        }
    }
}

/// 下载一次（从断点）：本地已有长度即断点（本地比远端长 = 脏续写，截断重传）
pub(crate) async fn download_once(
    sftp: Arc<SftpClient>,
    t: Arc<TransferInner>,
) -> Result<(), SftpError> {
    let (local, remote, on_exists, total) = {
        let info = lock(&t.info);
        (
            info.local.clone(),
            info.remote.clone(),
            info.on_exists,
            info.bytes_total,
        )
    };
    let local_len = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
    let offset = resume_offset(local_len, total, on_exists);
    t.bytes_done.store(offset, Ordering::Relaxed);

    if let Some(parent) = local.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SftpError::LocalIo {
            path: parent.display().to_string(),
            reason: e.to_string(),
        })?;
    }
    let mut dst = tokio::fs::OpenOptions::new()
        .create(true)
        .append(offset > 0)
        .truncate(offset == 0)
        .write(true)
        .open(&local)
        .await
        .map_err(|e| SftpError::LocalIo {
            path: local.display().to_string(),
            reason: e.to_string(),
        })?;

    // 已知全长且余量超过一块 → 显式 offset 流水线读（PR-16 P4-1）；
    // 无裸会话/小文件/未知全长 → 原串行路径
    let remaining = total.saturating_sub(offset);
    match sftp.download_raw().filter(|_| remaining > CHUNK as u64) {
        Some(raw) => {
            download_pipelined(raw, &remote, &local, &t, &mut dst, offset, total).await?;
        }
        None => {
            let mut src = sftp.open_read(&remote).await?;
            if offset > 0 {
                src.seek(std::io::SeekFrom::Start(offset))
                    .await
                    .map_err(|e| SftpError::LocalIo {
                        path: remote.clone(),
                        reason: e.to_string(),
                    })?;
            }
            let mut buf = vec![0u8; CHUNK];
            loop {
                if t.cancel.load(Ordering::Relaxed) || t.pause.load(Ordering::Relaxed) {
                    return Err(SftpError::Interrupted {
                        done: t.bytes_done.load(Ordering::Relaxed),
                        total: lock(&t.info).bytes_total,
                    });
                }
                let n = src
                    .read(&mut buf)
                    .await
                    .map_err(|e| SftpError::RemotePath {
                        path: remote.clone(),
                        reason: e.to_string(),
                    })?;
                if n == 0 {
                    break;
                }
                dst.write_all(&buf[..n])
                    .await
                    .map_err(|e| SftpError::LocalIo {
                        path: local.display().to_string(),
                        reason: e.to_string(),
                    })?;
                t.bytes_done.fetch_add(n as u64, Ordering::Relaxed);
            }
        }
    }
    dst.flush().await.map_err(|e| SftpError::LocalIo {
        path: local.display().to_string(),
        reason: e.to_string(),
    })?;
    Ok(())
}

/// 显式 offset 流水线读（PR-16 P4-1）：固定窗口在途 READ_AHEAD_DEPTH 个 f_read，
/// 乱序完成 → BTreeMap 重排 → 只落盘连续前缀。
/// 风险对策（对应子设计风险清单）：
/// - 乱序：重排缓冲，bytes_done 恒等于已落盘连续前缀，暂停/失败后 resume 安全
/// - 短包：SFTP 允许短读（仅 0 = EOF）——缺口尾部重新入队补齐
/// - EOF/服务端缩水：total 由 stat 预知，请求区间精确覆盖 [offset,total)，正常不见 EOF；
///   读到 0 = 服务端缩水 → 停发新请求，落定已有前缀后按完成收尾（与串行 EOF 语义一致）
/// - 中途失败：JoinSet 全 abort，错误上抛由队列重试（重试从 bytes_done 续）
/// - 取消/暂停：每轮等待前检查，中断点 = 已落盘连续前缀；在途请求 abort
async fn download_pipelined(
    raw: Arc<russh_sftp::client::RawSftpSession>,
    remote: &str,
    local: &std::path::Path,
    t: &Arc<TransferInner>,
    dst: &mut tokio::fs::File,
    offset: u64,
    total: u64,
) -> Result<(), SftpError> {
    let handle = raw
        .open(
            remote.to_string(),
            russh_sftp::protocol::OpenFlags::READ,
            russh_sftp::protocol::FileAttributes::default(),
        )
        .await
        .map_err(|e| SftpError::RemotePath {
            path: remote.to_string(),
            reason: e.to_string(),
        })?
        .handle;
    let result = pipelined_body(&raw, &handle, remote, local, t, dst, offset, total).await;
    // 句柄尽力关闭（不遮蔽业务结果；在途读已被 abort/排空）
    let _ = raw.close(handle).await;
    result
}

/// 在途读结果：(请求偏移, 请求长度, 数据)
type ReadDone = Result<(u64, u64, Vec<u8>), String>;

#[allow(clippy::too_many_arguments)]
async fn pipelined_body(
    raw: &Arc<russh_sftp::client::RawSftpSession>,
    handle: &str,
    remote: &str,
    local: &std::path::Path,
    t: &Arc<TransferInner>,
    dst: &mut tokio::fs::File,
    offset: u64,
    total: u64,
) -> Result<(), SftpError> {
    let mut next = offset; // 下一个待发请求偏移
    let mut write_at = offset; // 已落盘游标（连续前缀）
    let mut in_flight: tokio::task::JoinSet<ReadDone> = tokio::task::JoinSet::new();
    let mut reorder: std::collections::BTreeMap<u64, Vec<u8>> = std::collections::BTreeMap::new();
    // 短读缺口补读队列（(偏移, 长度)，恒在已请求区间内）
    let mut missing: std::collections::VecDeque<(u64, u64)> = std::collections::VecDeque::new();
    let mut shrink_eof = false; // 服务端缩水：停发新请求

    loop {
        // 补满窗口：先补短读缺口，再发新区间
        while !shrink_eof && in_flight.len() < READ_AHEAD_DEPTH {
            let (at, len) = match missing.pop_front() {
                Some(m) => m,
                None if next < total => {
                    let len = (total - next).min(CHUNK as u64);
                    next += len;
                    (next - len, len)
                }
                None => break,
            };
            let raw2 = raw.clone();
            let h = handle.to_string();
            in_flight.spawn(async move {
                raw2.read(h, at, len as u32)
                    .await
                    .map(|d| (at, len, d.data))
                    .map_err(|e| e.to_string())
            });
        }
        if in_flight.is_empty() {
            break;
        }
        // 取消/暂停：中断点 = 已落盘连续前缀
        if t.cancel.load(Ordering::Relaxed) || t.pause.load(Ordering::Relaxed) {
            in_flight.abort_all();
            return Err(SftpError::Interrupted {
                done: write_at,
                total,
            });
        }
        match in_flight.join_next().await {
            Some(Ok(Ok((at, want, data)))) => {
                if data.is_empty() {
                    shrink_eof = true; // 服务端缩水：后续请求不再发
                    continue;
                }
                let got = data.len() as u64;
                reorder.insert(at, data);
                if got < want {
                    missing.push_back((at + got, want - got)); // 短读补尾
                }
            }
            Some(Ok(Err(reason))) => {
                in_flight.abort_all();
                return Err(SftpError::RemotePath {
                    path: remote.to_string(),
                    reason,
                });
            }
            Some(Err(join_err)) => {
                in_flight.abort_all();
                return Err(SftpError::RemotePath {
                    path: remote.to_string(),
                    reason: join_err.to_string(),
                });
            }
            None => break,
        }
        // 只落盘连续前缀
        while reorder
            .first_key_value()
            .is_some_and(|(&at, _)| at == write_at)
        {
            let Some((_, data)) = reorder.pop_first() else {
                break;
            };
            dst.write_all(&data).await.map_err(|e| SftpError::LocalIo {
                path: local.display().to_string(),
                reason: e.to_string(),
            })?;
            write_at += data.len() as u64;
        }
        t.bytes_done.store(write_at, Ordering::Relaxed);
    }
    Ok(())
}

/// 上传一次（从断点）：远端已有长度即断点（stat 失败视为 0；远端比本地长 = 脏续写，截断重传）
pub(crate) async fn upload_once(
    sftp: Arc<SftpClient>,
    t: Arc<TransferInner>,
) -> Result<(), SftpError> {
    let (local, remote, on_exists, local_size) = {
        let info = lock(&t.info);
        (
            info.local.clone(),
            info.remote.clone(),
            info.on_exists,
            info.bytes_total,
        )
    };
    let remote_size = sftp.stat(&remote).await.map(|s| s.size).unwrap_or(0);
    let offset = resume_offset(remote_size, local_size, on_exists);
    t.bytes_done.store(offset, Ordering::Relaxed);

    let mut src = tokio::fs::File::open(&local)
        .await
        .map_err(|e| SftpError::LocalIo {
            path: local.display().to_string(),
            reason: e.to_string(),
        })?;
    if offset > 0 {
        src.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| SftpError::LocalIo {
                path: local.display().to_string(),
                reason: e.to_string(),
            })?;
    }
    let mut dst = sftp.open_write_at(&remote, offset).await?;

    let mut buf = vec![0u8; CHUNK];
    loop {
        if t.cancel.load(Ordering::Relaxed) || t.pause.load(Ordering::Relaxed) {
            return Err(SftpError::Interrupted {
                done: t.bytes_done.load(Ordering::Relaxed),
                total: lock(&t.info).bytes_total,
            });
        }
        let n = src.read(&mut buf).await.map_err(|e| SftpError::LocalIo {
            path: local.display().to_string(),
            reason: e.to_string(),
        })?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n])
            .await
            .map_err(|e| SftpError::RemotePath {
                path: remote.clone(),
                reason: e.to_string(),
            })?;
        t.bytes_done.fetch_add(n as u64, Ordering::Relaxed);
    }
    // 关键：shutdown 排空 fire-and-forget 写确认，否则尾包可能未落地
    dst.shutdown().await.map_err(|e| SftpError::RemotePath {
        path: remote.clone(),
        reason: e.to_string(),
    })?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn resume_offset_resume_uses_existing_length() {
        assert_eq!(resume_offset(40, 100, OnExists::Resume), 40);
        assert_eq!(resume_offset(0, 100, OnExists::Resume), 0);
        // 已完成（等长）：偏移即全长，开跑后立即 EOF
        assert_eq!(resume_offset(100, 100, OnExists::Resume), 100);
    }

    #[test]
    fn resume_offset_guardrail_resets_when_target_longer() {
        // 目标比源长 = 内容不符，归零重传（resume/rename 的运行期语义都走这里）
        assert_eq!(resume_offset(120, 100, OnExists::Resume), 0);
        assert_eq!(resume_offset(120, 100, OnExists::Rename), 0);
        assert_eq!(resume_offset(120, 100, OnExists::Skip), 0);
    }

    #[test]
    fn resume_offset_overwrite_always_zero() {
        assert_eq!(resume_offset(0, 100, OnExists::Overwrite), 0);
        assert_eq!(resume_offset(50, 100, OnExists::Overwrite), 0);
        assert_eq!(resume_offset(200, 100, OnExists::Overwrite), 0);
    }

    #[test]
    fn rename_candidate_appends_counter_before_ext() {
        assert_eq!(
            rename_candidate("/data/report.csv", 1),
            "/data/report-1.csv"
        );
        assert_eq!(
            rename_candidate("/data/report.csv", 12),
            "/data/report-12.csv"
        );
        // 无目录 / 无扩展名 / 隐藏文件
        assert_eq!(rename_candidate("report.csv", 2), "report-2.csv");
        assert_eq!(rename_candidate("/data/Makefile", 1), "/data/Makefile-1");
        assert_eq!(rename_candidate("/data/.env", 3), "/data/.env-3");
        // 多级扩展名只认最后一段；Windows 分隔符
        assert_eq!(
            rename_candidate("C:/dl/report.tar.gz", 1),
            "C:/dl/report.tar-1.gz"
        );
        assert_eq!(
            rename_candidate("C:\\dl\\report.csv", 1),
            "C:\\dl\\report-1.csv"
        );
    }

    #[test]
    fn on_exists_serde_default_is_resume() {
        // 历史行缺 onExists 字段时反序列化为 resume（既有续传行为）
        #[derive(serde::Deserialize)]
        struct Row {
            #[serde(default)]
            on_exists: OnExists,
        }
        let row: Row = serde_json::from_str("{}").expect("缺字段应回退默认值");
        assert_eq!(row.on_exists, OnExists::Resume);
        // 四模式字面量往返
        for (s, m) in [
            ("resume", OnExists::Resume),
            ("overwrite", OnExists::Overwrite),
            ("skip", OnExists::Skip),
            ("rename", OnExists::Rename),
        ] {
            let v: OnExists = serde_json::from_str(&format!("\"{s}\"")).expect("合法策略字面量");
            assert_eq!(v, m);
            assert_eq!(v.as_str(), s);
        }
        assert!(serde_json::from_str::<OnExists>("\"bogus\"").is_err());
    }

    #[test]
    fn on_exists_runtime_normalizes_to_resume_or_overwrite() {
        assert_eq!(OnExists::Resume.runtime(), OnExists::Resume);
        assert_eq!(OnExists::Overwrite.runtime(), OnExists::Overwrite);
        assert_eq!(OnExists::Skip.runtime(), OnExists::Resume);
        assert_eq!(OnExists::Rename.runtime(), OnExists::Resume);
    }
}
