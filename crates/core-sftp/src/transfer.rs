//! 队列化传输：并发可控、断点续传、失败重试、暂停/取消、进度回调。
//!
//! 设计要点：
//! - 并发上限用 Semaphore（默认 3），超过排队
//! - 每个传输 = 独立 tokio 任务，分块 256KB（对齐 russh-sftp max_packet_len）
//! - 续传：下载看本地已有长度；上传先 stat 远端长度，从断点继续
//! - 重试：失败自动重试（默认 2 次），每次从当前断点继续
//! - 终态条目支持手动重试（retry）/移除（remove）/批量清理（clear_where）
//! - russh-sftp 写为 fire-and-forget，完成前必须 shutdown 排空写确认
//!   （否则最后若干包可能未落地——集成测试踩过）
//! - 进度经回调外发（app 层接 Channel 推送 UI）；速率由调用方按采样算

use crate::{SftpClient, SftpError, TransferDirection};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;
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

struct TransferInner {
    info: Mutex<TransferInfo>,
    bytes_done: AtomicU64,
    pause: AtomicBool,
    cancel: AtomicBool,
}

/// 进度回调（每个分块落地后调用；实现必须快，不得阻塞）
pub type ProgressFn = Arc<dyn Fn(TransferInfo) + Send + Sync>;

pub struct TransferQueue {
    sftp: Arc<SftpClient>,
    /// 传输任务落点（app 传 bulk-rt 的 Handle，保 runtime 分离铁律）
    rt: tokio::runtime::Handle,
    permits: Arc<Semaphore>,
    transfers: Mutex<HashMap<TransferId, Arc<TransferInner>>>,
    id_seq: AtomicU64,
    max_retries: u32,
    on_progress: Mutex<Option<ProgressFn>>,
}

const CHUNK: usize = 256 * 1024;

/// 锁中毒自愈（panic 现场已恢复，数据本身无损坏语义）
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl TransferQueue {
    pub fn new(sftp: Arc<SftpClient>, max_concurrent: usize, rt: tokio::runtime::Handle) -> Self {
        Self {
            sftp,
            rt,
            permits: Arc::new(Semaphore::new(max_concurrent.max(1))),
            transfers: Mutex::new(HashMap::new()),
            id_seq: AtomicU64::new(1),
            max_retries: 2,
            on_progress: Mutex::new(None),
        }
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

    /// 通用执行器：并发闸 + 暂停自旋 + 断点重试环
    async fn run_transfer(&self, t: Arc<TransferInner>) {
        let direction = lock(&t.info).direction;
        let _permit = self
            .permits
            .acquire()
            .await
            .unwrap_or_else(|_| unreachable!("semaphore closed"));
        loop {
            if t.cancel.load(Ordering::Relaxed) {
                lock(&t.info).state = TransferState::Canceled;
                self.emit(&t);
                return;
            }
            // 暂停自旋（200ms 粒度；传输块大，粒度不敏感）
            while t.pause.load(Ordering::Relaxed) && !t.cancel.load(Ordering::Relaxed) {
                lock(&t.info).state = TransferState::Paused;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            if t.cancel.load(Ordering::Relaxed) {
                continue; // 交由顶部 cancel 分支
            }
            lock(&t.info).state = TransferState::Running;
            self.emit(&t);
            let result = match direction {
                TransferDirection::Download => download_once(self.sftp.clone(), t.clone()).await,
                TransferDirection::Upload => upload_once(self.sftp.clone(), t.clone()).await,
            };
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
                    // 暂停引发的断点中断不算失败重试，回暂停自旋
                    if t.pause.load(Ordering::Relaxed) {
                        continue;
                    }
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
                    // 重试退避：1s、2s
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
async fn download_once(sftp: Arc<SftpClient>, t: Arc<TransferInner>) -> Result<(), SftpError> {
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

    let mut src = sftp.open_read(&remote).await?;
    if offset > 0 {
        src.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| SftpError::LocalIo {
                path: remote.clone(),
                reason: e.to_string(),
            })?;
    }
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

    let mut buf = vec![0u8; CHUNK];
    loop {
        if t.cancel.load(Ordering::Relaxed) {
            return Err(SftpError::Interrupted {
                done: t.bytes_done.load(Ordering::Relaxed),
                total: lock(&t.info).bytes_total,
            });
        }
        if t.pause.load(Ordering::Relaxed) {
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
    dst.flush().await.map_err(|e| SftpError::LocalIo {
        path: local.display().to_string(),
        reason: e.to_string(),
    })?;
    Ok(())
}

/// 上传一次（从断点）：远端已有长度即断点（stat 失败视为 0；远端比本地长 = 脏续写，截断重传）
async fn upload_once(sftp: Arc<SftpClient>, t: Arc<TransferInner>) -> Result<(), SftpError> {
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
