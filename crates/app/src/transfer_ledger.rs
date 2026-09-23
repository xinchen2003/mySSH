//! 传输账本（TransferLedger，ADR-0002）：transfers 表的唯一权威。
//!
//! 两条显式契约：
//! - 终态/暂停才落库（paused/done/failed/canceled）；Queued/Running 纯内存。
//! - 一切变更经单一有序流执行：upsert 可丢（满即丢+计数，ADR-0001 边⑥红线：
//!   SQLite 慢不卡传输）；remove/clear 可靠入队，与先前排队的 upsert 严格保序——
//!   「排队中的 upsert 反超删除」这一复活变体在顺序层面不可表示。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use core_store::{Store, TransferRecord};
use tokio::sync::{mpsc, oneshot};

/// ADR-0001 边⑥：变更队列上限
const QUEUE_CAP: usize = 4096;

/// 单批 upsert 上限（沿用 ADR-0001 history writer 的批量）
const BATCH: usize = 500;

/// 落库白名单（终态/暂停契约的唯一执行点）
fn settled(state: core_sftp::TransferState) -> bool {
    use core_sftp::TransferState::*;
    matches!(state, Paused | Done | Failed | Canceled)
}

/// 一条终态/暂停记录（账本写入口的输入；由 TransferInfo / FileTerminal 转换而来）
pub struct TerminalEntry {
    pub session_id: String,
    pub id: String,
    pub direction: core_sftp::TransferDirection,
    pub local: std::path::PathBuf,
    pub remote: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub state: core_sftp::TransferState,
    pub error: Option<String>,
}

impl TerminalEntry {
    /// 单文件传输生命周期回调入口：非终态/暂停返回 None（契约过滤）
    pub fn from_info(session_id: &str, info: &core_sftp::TransferInfo) -> Option<Self> {
        settled(info.state).then(|| Self {
            session_id: session_id.to_string(),
            id: info.id.clone(),
            direction: info.direction,
            local: info.local.clone(),
            remote: info.remote.clone(),
            bytes_done: info.bytes_done,
            bytes_total: info.bytes_total,
            state: info.state,
            error: info.error.clone(),
        })
    }

    /// DirectoryJob 逐文件终态入口（worker fire-and-forget）
    pub fn from_file_terminal(session_id: &str, rec: core_sftp::FileTerminal) -> Option<Self> {
        settled(rec.state).then(|| Self {
            session_id: session_id.to_string(),
            id: rec.id,
            direction: rec.direction,
            local: rec.local,
            remote: rec.remote,
            bytes_done: rec.bytes_done,
            bytes_total: rec.bytes_total,
            state: rec.state,
            error: rec.error,
        })
    }

    fn into_record(self) -> TransferRecord {
        TransferRecord {
            id: self.id,
            session_id: self.session_id,
            direction: match self.direction {
                core_sftp::TransferDirection::Upload => "upload".into(),
                core_sftp::TransferDirection::Download => "download".into(),
            },
            local: self.local.to_string_lossy().into_owned(),
            remote: self.remote,
            bytes_done: self.bytes_done,
            bytes_total: self.bytes_total,
            state: self.state.as_str().into(),
            error: self.error,
            updated_at: String::new(), // 写入侧由 SQLite 时钟生成
        }
    }
}

/// transfer_clear 的过滤口径
#[derive(Debug, Clone, Copy)]
pub enum ClearScope {
    Done,
    /// failed 含 canceled
    Failed,
}

impl ClearScope {
    fn states(self) -> &'static [&'static str] {
        match self {
            Self::Done => &["done"],
            Self::Failed => &["failed", "canceled"],
        }
    }
}

type Ack<T> = oneshot::Sender<Result<T, String>>;

enum Mutation {
    Upsert(Box<TransferRecord>),
    /// 删除并返回被删行（调用方据此清理残件）
    Remove {
        id: String,
        ack: Ack<Option<TransferRecord>>,
    },
    /// 按会话+状态集合删除，返回被删行
    ClearSession {
        session_id: String,
        states: &'static [&'static str],
        ack: Ack<Vec<TransferRecord>>,
    },
    ClearAll {
        ack: Ack<u64>,
    },
    /// 有序读：排在在途 upsert 之后执行，保证看见已落队的 paused 行
    Resumable {
        id: String,
        session_id: String,
        ack: Ack<Option<TransferRecord>>,
    },
}

/// transfers 表的唯一权威。Clone 廉价（通道句柄）。
#[derive(Clone)]
pub struct TransferLedger {
    tx: mpsc::Sender<Mutation>,
    store: Arc<Store>,
    dropped: Arc<AtomicU64>,
}

impl TransferLedger {
    /// 启动账本（消费者跑在给定 runtime 上——生产为 bulk-rt，测试为当前 runtime）
    pub fn start(store: Arc<Store>, rt: &tokio::runtime::Handle) -> Self {
        let (tx, rx) = mpsc::channel(QUEUE_CAP);
        let dropped = Arc::new(AtomicU64::new(0));
        rt.spawn(consume(store.clone(), rx));
        Self { tx, store, dropped }
    }

    /// 终态/暂停落库的唯一写入口。可丢：队列满即丢弃并计数（传输不得被 SQLite 卡住）。
    pub fn on_terminal(&self, entry: TerminalEntry) {
        if !settled(entry.state) {
            // 契约违背（绕过 from_* 构造过滤直接构造 entry）：拒绝落库
            tracing::warn!(id = %entry.id, state = %entry.state.as_str(), "传输账本拒绝非终态落库");
            return;
        }
        if self
            .tx
            .try_send(Mutation::Upsert(Box::new(entry.into_record())))
            .is_err()
        {
            let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // 洪泛期逐条 warn 即日志风暴：首条 + 每 4096 条报一次
            if n == 1 || n.is_multiple_of(4096) {
                tracing::warn!(dropped = n, "传输账本队列满，文件级记录丢弃累计");
            }
        }
    }
    /// 因队列满丢弃的 upsert 累计数（可观测性）
    pub fn dropped_count(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 有序删除：与先前排队的 upsert 保序。返回被删行（无则 None）。
    pub async fn remove(&self, id: &str) -> Result<Option<TransferRecord>, String> {
        let (ack, rx) = oneshot::channel();
        self.send_reliable(Mutation::Remove { id: id.into(), ack })
            .await?;
        rx.await.map_err(|_| "传输账本已关闭".to_string())?
    }

    /// 有序按会话清理（done / failed+canceled），返回被删行。
    pub async fn clear_session(
        &self,
        session_id: &str,
        scope: ClearScope,
    ) -> Result<Vec<TransferRecord>, String> {
        let (ack, rx) = oneshot::channel();
        self.send_reliable(Mutation::ClearSession {
            session_id: session_id.into(),
            states: scope.states(),
            ack,
        })
        .await?;
        rx.await.map_err(|_| "传输账本已关闭".to_string())?
    }

    /// 清空全部会话历史（有序）。
    pub async fn clear_all(&self) -> Result<u64, String> {
        let (ack, rx) = oneshot::channel();
        self.send_reliable(Mutation::ClearAll { ack }).await?;
        rx.await.map_err(|_| "传输账本已关闭".to_string())?
    }

    /// resume 凭据：有序读，保证看见已落队的 paused 行；只认本会话 paused。
    pub async fn resumable(
        &self,
        session_id: &str,
        id: &str,
    ) -> Result<Option<TransferRecord>, String> {
        let (ack, rx) = oneshot::channel();
        self.send_reliable(Mutation::Resumable {
            id: id.into(),
            session_id: session_id.into(),
            ack,
        })
        .await?;
        rx.await.map_err(|_| "传输账本已关闭".to_string())?
    }

    /// 会话历史行（transfer_list 合并用）。直读：弱 200ms 级可见性可接受，
    /// 进行中/暂停的传输由 live 注册表覆盖，不依赖本读的新鲜度。
    pub async fn session_rows(&self, session_id: &str) -> Result<Vec<TransferRecord>, String> {
        self.store
            .transfers()
            .for_session(session_id)
            .await
            .map_err(|e| e.to_string())
    }

    /// 全部会话历史（TransferCenter 历史区），按更新时间倒序。直读。
    pub async fn history(&self, limit: u32) -> Result<Vec<TransferRecord>, String> {
        self.store
            .transfers()
            .recent(limit)
            .await
            .map_err(|e| e.to_string())
    }

    /// 可靠变更入队：await 背压（remove/clear 为用户发起的低频操作，允许等）
    async fn send_reliable(&self, m: Mutation) -> Result<(), String> {
        self.tx
            .send(m)
            .await
            .map_err(|_| "传输账本已关闭".to_string())
    }
}

/// 单消费者：严格 FIFO。连续 upsert 聚批写；可靠变更前先把聚批落库（保序）。
/// 通道关闭（全部发送侧 drop）前 recv 会先交付全部已排队消息，无残留。
async fn consume(store: Arc<Store>, mut rx: mpsc::Receiver<Mutation>) {
    let mut upserts: Vec<TransferRecord> = Vec::with_capacity(BATCH);
    while let Some(first) = rx.recv().await {
        let mut n = 1usize;
        let mut m = first;
        loop {
            match m {
                Mutation::Upsert(r) => upserts.push(*r),
                other => {
                    flush(&store, &mut upserts).await;
                    exec(&store, other).await;
                }
            }
            if n >= BATCH {
                break;
            }
            match rx.try_recv() {
                Ok(next) => {
                    m = next;
                    n += 1;
                }
                Err(_) => break,
            }
        }
        flush(&store, &mut upserts).await;
    }
}

async fn flush(store: &Store, upserts: &mut Vec<TransferRecord>) {
    for r in upserts.drain(..) {
        if let Err(e) = store.transfers().upsert(&r).await {
            tracing::warn!(id = %r.id, error = %e, "传输终态落库失败");
        }
    }
}

async fn exec(store: &Store, m: Mutation) {
    let repo = store.transfers();
    match m {
        Mutation::Upsert(r) => {
            // 构造上 upsert 在 consume 聚批不进 exec；防御性直写
            if let Err(e) = repo.upsert(&r).await {
                tracing::warn!(id = %r.id, error = %e, "传输终态落库失败");
            }
        }
        Mutation::Remove { id, ack } => {
            let r = async {
                let row = repo.get(&id).await.map_err(|e| e.to_string())?;
                if row.is_some() {
                    repo.delete(&id).await.map_err(|e| e.to_string())?;
                }
                Ok(row)
            }
            .await;
            let _ = ack.send(r);
        }
        Mutation::ClearSession {
            session_id,
            states,
            ack,
        } => {
            let r = async {
                let rows = repo
                    .for_session(&session_id)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut hit = Vec::new();
                for row in rows {
                    if states.contains(&row.state.as_str()) {
                        repo.delete(&row.id).await.map_err(|e| e.to_string())?;
                        hit.push(row);
                    }
                }
                Ok(hit)
            }
            .await;
            let _ = ack.send(r);
        }
        Mutation::ClearAll { ack } => {
            let _ = ack.send(repo.clear_all().await.map_err(|e| e.to_string()));
        }
        Mutation::Resumable {
            id,
            session_id,
            ack,
        } => {
            let r = repo
                .get(&id)
                .await
                .map_err(|e| e.to_string())
                .map(|row| row.filter(|r| r.session_id == session_id && r.state == "paused"));
            let _ = ack.send(r);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    use core_sftp::{TransferDirection, TransferState};

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("myssh-ledger-{tag}-{}-{n}.db", std::process::id()))
    }

    async fn ledger(tag: &str) -> (TransferLedger, std::path::PathBuf) {
        let path = temp_db(tag);
        let _ = std::fs::remove_file(&path);
        let store = Arc::new(Store::open(&path).await.expect("open store"));
        (
            TransferLedger::start(store, &tokio::runtime::Handle::current()),
            path,
        )
    }

    fn entry(id: &str, session: &str, state: TransferState) -> TerminalEntry {
        TerminalEntry {
            session_id: session.into(),
            id: id.into(),
            direction: TransferDirection::Download,
            local: std::path::PathBuf::from("C:/tmp/f.bin"),
            remote: "/srv/f.bin".into(),
            bytes_done: 10,
            bytes_total: 100,
            state,
            error: None,
        }
    }

    /// 复活防护：upsert 在飞时 remove——有序流下 remove 必见该行，
    /// 且删除后任何读都不得再见到它（bc095b1 的回归钉）。
    #[tokio::test]
    async fn remove_orders_after_inflight_upsert() {
        let (ledger, path) = ledger("order").await;
        ledger.on_terminal(entry("t1", "s1", TransferState::Paused));
        let removed = ledger.remove("t1").await.expect("remove");
        assert!(removed.is_some(), "有序流：remove 必看见先排队的 upsert");
        assert!(ledger.history(10).await.expect("history").is_empty());
        assert!(
            ledger.resumable("s1", "t1").await.expect("res").is_none(),
            "删除后不得复活"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 终态契约：Queued/Running 不落库，只有终态/暂停落库。
    #[tokio::test]
    async fn only_settled_states_persist() {
        let (ledger, path) = ledger("settled").await;
        ledger.on_terminal(entry("q", "s1", TransferState::Queued));
        ledger.on_terminal(entry("r", "s1", TransferState::Running));
        ledger.on_terminal(entry("d", "s1", TransferState::Done));
        // 以一次有序读作屏障：排在全部已入队 upsert 之后
        let _ = ledger.resumable("s1", "d").await.expect("barrier");
        let rows = ledger.history(10).await.expect("history");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "d");
        let _ = std::fs::remove_file(&path);
    }

    /// resume 凭据：只认本会话 paused 行；他会话/非 paused 一律 None。
    #[tokio::test]
    async fn resumable_filters_session_and_paused() {
        let (ledger, path) = ledger("resume").await;
        ledger.on_terminal(entry("p", "s1", TransferState::Paused));
        ledger.on_terminal(entry("f", "s1", TransferState::Failed));
        ledger.on_terminal(entry("x", "s2", TransferState::Paused));
        let _ = ledger.resumable("s1", "p").await.expect("barrier");
        assert!(ledger.resumable("s1", "p").await.expect("r").is_some());
        assert!(ledger.resumable("s2", "p").await.expect("r").is_none());
        assert!(ledger.resumable("s1", "f").await.expect("r").is_none());
        let _ = std::fs::remove_file(&path);
    }

    /// clear_session 口径：failed 含 canceled，且不动 paused 与他会话行。
    #[tokio::test]
    async fn clear_session_scope_and_isolation() {
        let (ledger, path) = ledger("clear").await;
        ledger.on_terminal(entry("d", "s1", TransferState::Done));
        ledger.on_terminal(entry("f", "s1", TransferState::Failed));
        ledger.on_terminal(entry("c", "s1", TransferState::Canceled));
        ledger.on_terminal(entry("p", "s1", TransferState::Paused));
        ledger.on_terminal(entry("o", "s2", TransferState::Failed));
        let hit = ledger
            .clear_session("s1", ClearScope::Failed)
            .await
            .expect("clear");
        assert_eq!(hit.len(), 2, "failed+canceled");
        let _ = ledger.resumable("s1", "p").await.expect("barrier");
        let rows = ledger.session_rows("s1").await.expect("rows");
        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"d") && ids.contains(&"p"), "done/paused 保留");
        let s2 = ledger.session_rows("s2").await.expect("rows");
        assert_eq!(s2.len(), 1, "他会话不受影响");
        let _ = std::fs::remove_file(&path);
    }
}
