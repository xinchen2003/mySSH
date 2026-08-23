//! 队列化传输：并发可控、断点续传、失败重试、暂停/取消、进度回调。
//!
//! 设计要点：
//! - 并发上限用 Semaphore（默认 3），超过排队
//! - 每个传输 = 独立 tokio 任务，分块 256KB（对齐 russh-sftp max_packet_len）
//! - 续传：下载看本地已有长度；上传先 stat 远端长度，从断点继续
//! - 重试：失败自动重试（默认 2 次），每次从当前断点继续
//! - russh-sftp 写为 fire-and-forget，完成前必须 shutdown 排空写确认
//!   （否则最后若干包可能未落地——集成测试踩过）
//! - 进度经回调外发（app 层接 Channel 推送 UI）；速率由调用方按采样算

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use crate::{SftpClient, SftpError, TransferDirection};

pub type TransferId = String;

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
    pub fn new(sftp: Arc<SftpClient>, max_concurrent: usize) -> Self {
        Self {
            sftp,
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
    ) -> (TransferId, Arc<TransferInner>) {
        let id = format!("tr-{}", self.id_seq.fetch_add(1, Ordering::Relaxed));
        let inner = Arc::new(TransferInner {
            info: Mutex::new(TransferInfo {
                id: id.clone(),
                direction,
                local,
                remote,
                state: TransferState::Queued,
                bytes_done: 0,
                bytes_total,
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
    ) -> TransferId {
        self.enqueue(TransferDirection::Download, local, remote, bytes_total)
            .await
    }

    /// 入队上传（local -> remote）
    pub async fn enqueue_upload(
        self: &Arc<Self>,
        local: PathBuf,
        remote: String,
        bytes_total: u64,
    ) -> TransferId {
        self.enqueue(TransferDirection::Upload, local, remote, bytes_total)
            .await
    }

    async fn enqueue(
        self: &Arc<Self>,
        direction: TransferDirection,
        local: PathBuf,
        remote: String,
        bytes_total: u64,
    ) -> TransferId {
        let (id, inner) = self.register(direction, local, remote, bytes_total);
        let q = self.clone();
        tokio::spawn(async move {
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
}

/// 下载一次（从断点）：本地已有长度即断点
async fn download_once(sftp: Arc<SftpClient>, t: Arc<TransferInner>) -> Result<(), SftpError> {
    let (local, remote) = {
        let info = lock(&t.info);
        (info.local.clone(), info.remote.clone())
    };
    let offset = std::fs::metadata(&local).map(|m| m.len()).unwrap_or(0);
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

/// 上传一次（从断点）：远端已有长度即断点（stat 失败视为 0）
async fn upload_once(sftp: Arc<SftpClient>, t: Arc<TransferInner>) -> Result<(), SftpError> {
    let (local, remote) = {
        let info = lock(&t.info);
        (info.local.clone(), info.remote.clone())
    };
    let offset = sftp.stat(&remote).await.map(|s| s.size).unwrap_or(0);
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
