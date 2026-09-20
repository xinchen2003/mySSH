//! FsIoLimiter：把同步 `std::fs` 操作移出 async worker 的共享限流器。
//!
//! 三组 Semaphore 配额（性能优化 PR-3）：
//! - metadata/小读：16 —— stat/mkdir/touch/rename/小文件读，量大但单次极短；
//! - 递归扫描：2 —— 目录遍历，慢盘（网络盘/限流盘）上长尾明显，须严格限并发；
//! - 重拷贝/删除：2 —— 大文件整体写入/递归复制/递归删除，占满盘带宽的操作。
//!
//! 使用模式：先 `acquire_owned()` 拿 permit 再 `tokio::task::spawn_blocking`，
//! permit 随闭包结束自动释放。禁止在持有 async Mutex 时进入本模块
//! （permit 等待会拉长锁持有时长，阻塞同锁其他路径）。

use std::sync::{Arc, LazyLock};

use tokio::sync::Semaphore;

/// metadata/小读并发配额
const META_PERMITS: usize = 16;
/// 递归扫描并发配额
const SCAN_PERMITS: usize = 2;
/// 重拷贝/删除并发配额
const HEAVY_PERMITS: usize = 2;

struct FsIoLimiter {
    meta: Arc<Semaphore>,
    scan: Arc<Semaphore>,
    heavy: Arc<Semaphore>,
}

static LIMITER: LazyLock<FsIoLimiter> = LazyLock::new(|| FsIoLimiter {
    meta: Arc::new(Semaphore::new(META_PERMITS)),
    scan: Arc::new(Semaphore::new(SCAN_PERMITS)),
    heavy: Arc::new(Semaphore::new(HEAVY_PERMITS)),
});

async fn run<T>(sem: &Arc<Semaphore>, f: impl FnOnce() -> T + Send + 'static) -> Result<T, String>
where
    T: Send + 'static,
{
    // 限流器进程级常驻、从不 close，acquire 失败只会是 Semaphore::close（不存在）；
    // join 失败只会是闭包 panic/abort——属编程错误，如实回报。
    let permit = sem
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| "FS 限流器已关闭".to_string())?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .map_err(|e| format!("FS 任务执行失败: {e}"))
}

/// metadata/小读组：stat/mkdir/touch/rename/≤64KB 读等轻量操作
pub async fn metadata<T>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String>
where
    T: Send + 'static,
{
    run(&LIMITER.meta, f).await
}

/// 递归扫描组：目录遍历（PR-6 本地扫描复用此配额）
#[allow(dead_code)] // 配额按 PR-3 规格预置，消费方在 PR-6 落地
pub async fn scan<T>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String>
where
    T: Send + 'static,
{
    run(&LIMITER.scan, f).await
}

/// 重拷贝/删除组：递归复制、递归删除、大文件整体写入
pub async fn heavy<T>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String>
where
    T: Send + 'static,
{
    run(&LIMITER.heavy, f).await
}
