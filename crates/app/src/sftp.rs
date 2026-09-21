//! SFTP 命令族：sftp_*（浏览/元操作/直编）+ transfer_*（队列化传输）。
//!
//! 架构要点：
//! - 连接：每会话一条 Bulk 连接（与隧道同策略：KI 拒绝、known_hosts 严格）
//! - runtime 分离：bulk-rt 独立 std::thread（设计 06：批量 IO 不占交互 runtime）
//! - 传输：TransferQueue 任务经 rt.handle().spawn 落 bulk-rt
//! - 进度：transfer_subscribe 500ms 推快照；终态落 transfers 表（跨重启续传凭据）
//! - 远程直编：下载临时区 → 1s 轮询 mtime → 变更即回传（小文件语义，编辑器场景）

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tauri::ipc::Channel;

use core_sftp::{
    rename_candidate, DirEntry, DirectoryJobScheduler, EntryKind, FileTerminal, JobRoot,
    JobSnapshot, JobSpec, OnExists, SftpClient, SftpSlot, SubsystemSlot, TransferDirection,
    TransferQueue,
};
use core_store::Store;

use crate::sessions::SessionManagerState;

static EDIT_SEQ: AtomicU64 = AtomicU64::new(1);

/// 单会话的 SFTP 上下文（连接 + metadata/data 双 subsystem + 传输队列 + 目录任务调度器）
///
/// PR-11/C8 代际结构：Transport 代际 = 本 ctx（conn 死则 ensure_ctx 整体重建，
/// 两 slot 随之失效）；Metadata/Data 代际各自独立（slot 内单飞探活重建），
/// metadata 重建不影响 data 上的在途传输，data 重建由传输重试落新代际。
pub struct SftpCtx {
    /// 保活 + 监控复用（通道复用，不占交互连接）
    conn: Arc<core_ssh::SshConnection>,
    /// 浏览/元数据操作专用 subsystem
    metadata: Arc<SftpSlot>,
    /// 传输数据面专用 subsystem（queue 与 DirectoryJob 共用）
    data: Arc<SftpSlot>,
    queue: Arc<TransferQueue>,
    /// DirectoryJob 调度器（PR-8；与 queue 共享执行槽预算）
    jobs: Arc<DirectoryJobScheduler>,
}
/// ctx 级单飞槽（PR-12）：复用 SubsystemSlot 状态机——Connecting 单飞、
/// 恢复任务由槽持有（首 waiter 取消不连累）、失败退避 2s、代际不串扰
type CtxSlot = SubsystemSlot<SftpCtx>;

pub struct SftpManagerState {
    ctxs: Mutex<HashMap<String, Arc<CtxSlot>>>,
    rt: tokio::runtime::Handle,
}

impl SftpCtx {
    /// 监控复用同一 Bulk 连接（通道复用，不占交互连接）
    pub(crate) fn conn(&self) -> &core_ssh::SshConnection {
        &self.conn
    }

    /// MCP 传输工具复用（与 UI 同一队列：进度进 UI 传输面板、终态落 transfers 表）
    pub(crate) fn queue(&self) -> &Arc<TransferQueue> {
        &self.queue
    }

    /// 取 metadata 代际 client（多操作块一次取用；单操作优先走 meta() 记可疑）
    pub(crate) async fn meta_client(&self) -> Result<Arc<SftpClient>, String> {
        self.metadata.get().await.map_err(|e| e.to_string())
    }
}

/// 元数据操作统一入口（PR-11/C8）：取 metadata 代际 client 执行；
/// 失败即向 slot 上报可疑——下次取用先探活，探活失败单飞重建新代际
/// （远端业务错误探活通过则保留原代际，不引发重建）。
pub(crate) async fn meta<T, F, Fut>(ctx: &SftpCtx, f: F) -> Result<T, String>
where
    F: FnOnce(Arc<SftpClient>) -> Fut,
    Fut: std::future::Future<Output = Result<T, core_sftp::SftpError>>,
{
    let client = ctx.metadata.get().await.map_err(|e| e.to_string())?;
    match f(client).await {
        Ok(v) => Ok(v),
        Err(e) => {
            ctx.metadata.record_error();
            Err(e.to_string())
        }
    }
}

impl SftpManagerState {
    pub(crate) fn rt(&self) -> tokio::runtime::Handle {
        self.rt.clone()
    }
    /// 建 bulk-rt 线程（与 tunnel-rt 同构；设计 06 批量 runtime）
    pub fn new() -> Arc<Self> {
        let (tx, rx) = std::sync::mpsc::channel::<tokio::runtime::Handle>();
        std::thread::Builder::new()
            .name("bulk-rt".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .thread_name("bulk-worker")
                    .build()
                    .unwrap_or_else(|e| panic!("bulk runtime build: {e}"));
                tx.send(rt.handle().clone())
                    .unwrap_or_else(|_| panic!("bulk-rt handle send failed"));
                rt.block_on(std::future::pending::<()>());
            })
            .unwrap_or_else(|e| panic!("spawn bulk-rt thread: {e}"));
        let rt = rx
            .recv()
            .unwrap_or_else(|_| panic!("bulk runtime failed to start"));
        Arc::new(Self {
            ctxs: Mutex::new(HashMap::new()),
            rt,
        })
    }

    /// PR-0 可观测性：每会话上下文与传输队列快照
    pub(crate) fn perf_json(&self) -> Value {
        let ctxs = self.ctxs.lock();
        let sessions: Vec<Value> = ctxs
            .iter()
            .map(|(id, slot)| {
                let Some(ctx) = slot.ready() else {
                    return json!({ "sessionId": id, "slotState": slot.state_name() });
                };
                let tasks = ctx.queue.list();
                let (mut queued, mut running, mut paused) = (0usize, 0usize, 0usize);
                for t in &tasks {
                    match t.state {
                        core_sftp::TransferState::Queued => queued += 1,
                        core_sftp::TransferState::Running => running += 1,
                        core_sftp::TransferState::Paused => paused += 1,
                        _ => {}
                    }
                }
                json!({
                    "sessionId": id,
                    "slotState": slot.state_name(),
                    "ctxGen": slot.generation(),
                    "tasks": tasks.len(),
                    "queued": queued,
                    "running": running,
                    "paused": paused,
                    // PR-11/C8：subsystem 代际（重建次数可观测）
                    "metaGen": ctx.metadata.generation(),
                    "dataGen": ctx.data.generation(),
                })
            })
            .collect();
        json!({ "contexts": ctxs.len(), "sessions": sessions })
    }

    /// 会话配置变更/删除时摘除 ctx 槽（PR-12：旧组随配置失效；
    /// 槽 drop → ctx drop → Transport 与两 subsystem 关闭）
    pub(crate) fn drop_ctx(&self, session_id: &str) {
        self.ctxs.lock().remove(session_id);
    }
}

/// 取/建会话的 SFTP 上下文（PR-12 单飞：并发 ensure 只握手一次，
/// 连接任务由槽持有，失败 2s 退避后可重连，代际不串扰）
pub(crate) async fn ensure_ctx(
    state: &Arc<SftpManagerState>,
    store: &Arc<Store>,
    session_id: &str,
) -> Result<Arc<SftpCtx>, String> {
    let slot = {
        let mut map = state.ctxs.lock();
        match map.get(session_id) {
            Some(s) => s.clone(),
            None => {
                let s = new_ctx_slot(state, store, session_id);
                map.insert(session_id.to_string(), s.clone());
                s
            }
        }
    };
    // Transport 死亡即时代际失效（C8）：摘除 Ready，下次取用走单飞重建
    if let Some(ctx) = slot.ready() {
        if ctx.conn().is_closed() {
            slot.invalidate();
        }
    }
    slot.get().await.map_err(|e| e.to_string())
}

/// 建 ctx 槽：工厂闭包捕获 store/sid/rt——每次重建都重读会话配置
/// （配置变更经 drop_ctx 摘除旧槽后，新槽工厂拿到新配置）
fn new_ctx_slot(
    state: &Arc<SftpManagerState>,
    store: &Arc<Store>,
    session_id: &str,
) -> Arc<CtxSlot> {
    let rt = state.rt.clone();
    let store_f = store.clone();
    let sid = session_id.to_string();
    let slot = SubsystemSlot::new(
        "sftp-ctx",
        rt.clone(),
        Arc::new(move || {
            let store = store_f.clone();
            let sid = sid.clone();
            let rt = rt.clone();
            Box::pin(async move {
                build_ctx(&store, &sid, &rt)
                    .await
                    .map_err(core_sftp::SftpError::Subsystem)
            })
        }),
        // 探活 = Transport 存活性检查（无 RPC 成本）
        Arc::new(|ctx| {
            Box::pin(async move {
                if ctx.conn().is_closed() {
                    Err(core_sftp::SftpError::Subsystem("transport closed".into()))
                } else {
                    Ok(())
                }
            })
        }),
    );
    slot.set_retry_backoff(std::time::Duration::from_secs(2));
    slot
}

/// 完整 ctx 构建（连接 + 双 subsystem + 队列 + 调度器 + 落库/audit 接线），
/// 在 bulk-rt 上由槽的恢复任务执行——所有 waiter 拿到同一份接线完毕的 ctx
async fn build_ctx(
    store: &Arc<Store>,
    session_id: &str,
    rt: &tokio::runtime::Handle,
) -> Result<SftpCtx, String> {
    let spec = crate::sessions::resolve_session_spec(store, session_id).await?;
    if matches!(spec.auth, crate::terminal::AuthSpec::KeyboardInteractive)
        || spec
            .jump_chain
            .iter()
            .any(|h| matches!(h.auth, crate::terminal::AuthSpec::KeyboardInteractive))
    {
        return Err("keyboard-interactive 不适用于 SFTP 后台连接（请改用密钥/agent）".into());
    }
    let auth = crate::terminal::auth_method_from(&spec.auth);
    let conn = core_ssh::SshConnection::connect(core_ssh::ConnectOptions {
        host: spec.host.clone(),
        port: spec.port,
        user: spec.user.clone(),
        auth,
        jump_chain: crate::terminal::jump_chain_from(&spec.jump_chain),
        class: core_ssh::ConnClass::Bulk,
        window_size: 16 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: core_ssh::KeepaliveConfig::default(),
        host_key_check: crate::tunnels::tunnel_host_key_check(),
        ki_prompter: None,
    })
    .await
    .map_err(|e| e.to_string())?;
    // PR-11：同 Transport 两条 SFTP subsystem——metadata（浏览/元操作）
    // 与 data（传输数据面）各自独立代际，subsystem 级单飞重建（C8）
    let conn = Arc::new(conn);
    let handle = tokio::runtime::Handle::current();
    let metadata = SftpSlot::open_sftp("metadata", conn.clone(), handle.clone())
        .await
        .map_err(|e| e.to_string())?;
    let data = SftpSlot::open_sftp("data", conn.clone(), handle.clone())
        .await
        .map_err(|e| e.to_string())?;
    let queue = Arc::new(TransferQueue::new(data.clone(), 3, handle.clone()));
    let jobs = DirectoryJobScheduler::new(
        data.clone(),
        handle,
        queue.permits_handle(),
        crate::fs_limiter::scan_permits(),
    );
    // DirectoryJob 接线（ADR 0001 边 ⑤⑥）：writer 批量落库（fire-and-forget）+ job 终态 audit
    let (wtx, wrx) = tokio::sync::mpsc::channel::<FileTerminal>(4096);
    rt.spawn(history_writer(store.clone(), session_id.to_string(), wrx));
    jobs.set_file_terminal_callback(Arc::new(move |rec| {
        let _ = wtx.try_send(rec); // 满则丢弃文件级记录（红线：SQLite 慢不卡传输）
    }));
    {
        let store = store.clone();
        let sid = session_id.to_string();
        jobs.set_job_terminal_callback(Arc::new(move |snap| {
            let store = store.clone();
            let sid = sid.clone();
            let detail = format!(
                "{}（{}: 完成 {}/发现 {}，失败 {}，跳过 {}）",
                snap.summary,
                snap.state.as_str(),
                snap.completed_files,
                snap.discovered_files,
                snap.failed_files,
                snap.skipped
            );
            tauri::async_runtime::spawn(async move {
                audit(&store, &sid, "sftp_dir_job", &detail).await;
            });
        }));
    }
    Ok(SftpCtx {
        conn,
        metadata,
        data,
        queue,
        jobs,
    })
}

// ---------- 浏览与元操作 ----------

pub(crate) fn entry_to_json(e: &DirEntry) -> Value {
    json!({
        "name": e.name,
        "path": e.path,
        "kind": match e.kind {
            EntryKind::File => "file",
            EntryKind::Dir => "dir",
            EntryKind::Symlink => "symlink",
            EntryKind::Other => "other",
        },
        "size": e.size,
        "permissions": e.permissions,
        "mtime": e.mtime,
        "user": e.user,
        "group": e.group,
    })
}

#[tauri::command]
pub async fn sftp_list(
    session_id: String,
    path: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let entries = meta(&ctx, |c| async move { c.list(&path).await }).await?;
    Ok(json!({ "entries": entries.iter().map(entry_to_json).collect::<Vec<_>>() }))
}

#[tauri::command]
pub async fn sftp_stat(
    session_id: String,
    path: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let e = meta(&ctx, |c| async move { c.stat(&path).await }).await?;
    Ok(entry_to_json(&e))
}

#[tauri::command]
pub async fn sftp_mkdir(
    session_id: String,
    path: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    meta(&ctx, |c| {
        let p = &path;
        async move { c.mkdir(p).await }
    })
    .await?;
    audit(&sessions.store, &session_id, "sftp_mkdir", &path).await;
    Ok(())
}

#[tauri::command]
pub async fn sftp_delete(
    session_id: String,
    path: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    meta(&ctx, |c| {
        let p = &path;
        async move { c.remove_recursive(p).await }
    })
    .await?;
    audit(&sessions.store, &session_id, "sftp_delete", &path).await;
    Ok(())
}

#[tauri::command]
pub async fn sftp_rename(
    session_id: String,
    from: String,
    to: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    meta(&ctx, |c| {
        let f = &from;
        let t = &to;
        async move { c.rename(f, t).await }
    })
    .await?;
    audit(
        &sessions.store,
        &session_id,
        "sftp_rename",
        &format!("{from} -> {to}"),
    )
    .await;
    Ok(())
}

#[tauri::command]
pub async fn sftp_chmod(
    session_id: String,
    path: String,
    mode: u32,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    meta(&ctx, |c| {
        let p = &path;
        async move { c.chmod(p, mode).await }
    })
    .await?;
    audit(
        &sessions.store,
        &session_id,
        "sftp_chmod",
        &format!("{path} {mode:o}"),
    )
    .await;
    Ok(())
}

/// 远端新建空文件（已存在则报错，绝不截断）。
/// create 立即 shutdown + drop：无写数据，仅触发 CREATE 落地。
#[tauri::command]
pub async fn sftp_touch(
    session_id: String,
    path: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    // stat 探测是"不存在即可建"的正常流程（失败不记可疑）；open_write_at 是真操作
    let mc = ctx.metadata.get().await.map_err(|e| e.to_string())?;
    if mc.stat(&path).await.is_ok() {
        return Err(format!("目标已存在: {path}"));
    }
    use tokio::io::AsyncWriteExt;
    let mut f = match mc.open_write_at(&path, 0).await {
        Ok(f) => f,
        Err(e) => {
            ctx.metadata.record_error();
            return Err(e.to_string());
        }
    };
    f.shutdown().await.map_err(|e| e.to_string())?;
    audit(&sessions.store, &session_id, "sftp_touch", &path).await;
    Ok(())
}

// ---------- 本地浏览 ----------
/// 解析远端家目录绝对路径（SFTP 面板初始定位 / 权限失败回退用，批次六）。
/// 优先 expand-path@openssh.com 扩展（~ → .）；老服务器无此扩展时回退 REALPATH(.)
/// （SFTP v3 基础协议，均支持），解析 SFTP 会话默认起点即家目录的绝对路径。
pub(crate) async fn resolve_home_abs(client: &SftpClient) -> Result<String, String> {
    if let Some(p) = client.expand_path("~").await.map_err(|e| e.to_string())? {
        return Ok(p);
    }
    if let Some(p) = client.expand_path(".").await.map_err(|e| e.to_string())? {
        return Ok(p);
    }
    client.canonicalize(".").await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn sftp_home(
    session_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<String, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let mc = ctx.metadata.get().await.map_err(|e| e.to_string())?;
    resolve_home_abs(&mc).await
}

// ---------- Shell 集成（OSC 7 目录上报） ----------
// SFTP 面板「目录上报」开关的后端：往 ~/.bashrc / 已有的 ~/.zshrc 追加或剥离
// 标记块（core_sftp::shellint 纯函数）。走 SFTP 通道写文件，不触碰 shell
// 输入流——与「不向远程 shell 注入字节」的禁令不冲突。标记块即状态，不落库。

/// 读 rc 小文件；不存在 → None；存在但读失败 → 报错（绝不按空内容处理，
/// 否则 enable 会用纯集成块覆盖掉读不出的原文件）。上限 1 MiB 防异常。
async fn read_rc_file(client: &SftpClient, path: &str) -> Result<Option<String>, String> {
    use tokio::io::AsyncReadExt;
    if client.lstat(path).await.is_err() {
        return Ok(None);
    }
    let f = client.open_read(path).await.map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.take(1024 * 1024)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// 集成开关状态：~/.bashrc 或 ~/.zshrc 任一含标记块即视为已启用。
#[tauri::command]
pub async fn shell_integration_status(
    session_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let mc = ctx.metadata.get().await.map_err(|e| e.to_string())?;
    let home = resolve_home_abs(&mc).await?;
    let bash = read_rc_file(&mc, &format!("{home}/.bashrc")).await?;
    let zsh = read_rc_file(&mc, &format!("{home}/.zshrc")).await?;
    let enabled = [&bash, &zsh]
        .into_iter()
        .flatten()
        .any(|c| core_sftp::has_integration(c));
    Ok(json!({ "enabled": enabled }))
}

/// 开/关目录上报：~/.bashrc 总是目标（不存在则创建）；~/.zshrc 仅当服务器上
/// 已存在才碰（不给 zsh 用户塞 bash 文件，也不给 bash 用户新建 zshrc）。
#[tauri::command]
pub async fn shell_integration_set(
    session_id: String,
    enable: bool,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let mc = ctx.metadata.get().await.map_err(|e| e.to_string())?;
    // 本块真操作失败统一记可疑（lstat 探测属正常流程，不记）
    let rec = |e: core_sftp::SftpError| {
        ctx.metadata.record_error();
        e.to_string()
    };
    let home = resolve_home_abs(&mc).await?;
    let mut targets = vec![format!("{home}/.bashrc")];
    let zshrc = format!("{home}/.zshrc");
    if mc.lstat(&zshrc).await.is_ok() {
        targets.push(zshrc);
    }
    let mut touched: Vec<String> = Vec::new();
    for path in &targets {
        let content = read_rc_file(&mc, path).await?.unwrap_or_default();
        let next = if enable {
            core_sftp::add_integration(&content)
        } else {
            core_sftp::remove_integration(&content)
        };
        if next != content {
            mc.overwrite(path, next.as_bytes()).await.map_err(&rec)?;
            touched.push(path.clone());
        }
    }
    // 脚本本体 ~/.myssh/osc7.sh：enable 写入（目录不在则建），disable 删除。
    // rc 块与终端激活行都只是 source 它——用户在 rc/终端里看到的只有一行。
    let script = format!("{home}/{}", core_sftp::SCRIPT_REL);
    if enable {
        let dir = format!("{home}/.myssh");
        if mc.lstat(&dir).await.is_err() {
            mc.mkdir(&dir).await.map_err(&rec)?;
        }
        mc.overwrite(&script, core_sftp::SCRIPT.as_bytes())
            .await
            .map_err(&rec)?;
        touched.push(script);
    } else if mc.lstat(&script).await.is_ok() {
        mc.remove_file(&script).await.map_err(&rec)?;
        // 目录空则顺带收掉；非空（用户自己放了东西）remove_dir 会失败，忽略
        let _ = mc.remove_dir(&format!("{home}/.myssh")).await;
        touched.push(script);
    }
    audit(
        &sessions.store,
        &session_id,
        "shell_integration_set",
        &format!("enable={enable} touched={touched:?}"),
    )
    .await;
    Ok(json!({ "enabled": enable, "touched": touched }))
}

/// 本地目录列表（"" 或 "/" → Windows 盘符枚举）
#[tauri::command]
pub async fn local_list(path: String) -> Result<Value, String> {
    if path.is_empty() || path == "/" {
        let mut drives = Vec::new();
        for c in b'A'..=b'Z' {
            let d = format!("{}:/", c as char);
            if Path::new(&d).exists() {
                drives.push(json!({
                    "name": format!("{}:", c as char),
                    "path": d,
                    "kind": "dir",
                    "size": 0,
                    "mtime": null,
                }));
            }
        }
        return Ok(json!({ "entries": drives, "path": "" }));
    }
    let p = Path::new(&path);
    let rd = std::fs::read_dir(p).map_err(|e| format!("读取 {path} 失败: {e}"))?;
    let mut entries = Vec::new();
    for e in rd {
        let e = e.map_err(|e| e.to_string())?;
        let meta = e.metadata().map_err(|e| e.to_string())?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        entries.push(json!({
            "name": e.file_name().to_string_lossy(),
            "path": e.path().to_string_lossy().replace('\\', "/"),
            "kind": if meta.is_dir() { "dir" } else if meta.is_symlink() { "symlink" } else { "file" },
            "size": if meta.is_file() { meta.len() } else { 0 },
            "mtime": mtime,
        }));
    }
    // 目录在前，字典序
    entries.sort_by(|a, b| {
        let ad = a["kind"] == "dir";
        let bd = b["kind"] == "dir";
        bd.cmp(&ad).then_with(|| {
            a["name"]
                .as_str()
                .unwrap_or("")
                .to_lowercase()
                .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
        })
    });
    Ok(json!({ "entries": entries, "path": path.replace('\\', "/") }))
}

// ---------- 传输 ----------

/// 进度回调 → 终态落 transfers 表
fn persist_terminal(store: Arc<Store>, session_id: String) -> core_sftp::ProgressFn {
    Arc::new(move |info| {
        use core_sftp::TransferState::*;
        if !matches!(info.state, Done | Failed | Canceled) {
            return;
        }
        let store = store.clone();
        let session_id = session_id.clone();
        tauri::async_runtime::spawn(async move {
            let _ = store
                .transfers()
                .upsert(&core_store::TransferRecord {
                    id: info.id,
                    session_id,
                    direction: match info.direction {
                        TransferDirection::Upload => "upload".into(),
                        TransferDirection::Download => "download".into(),
                    },
                    local: info.local.to_string_lossy().to_string(),
                    remote: info.remote,
                    bytes_done: info.bytes_done,
                    bytes_total: info.bytes_total,
                    state: info.state.as_str().into(),
                    error: info.error,
                    updated_at: String::new(), // 写入侧由 SQLite 时钟生成
                })
                .await;
        });
    })
}

/// SQLite history writer（ADR 0001 边 ⑥）：200ms/500 条批量 upsert 逐文件终态。
/// 单任务独占写路径；回调侧 try_send，满即丢弃（红线：SQLite 慢不卡传输）。
async fn history_writer(
    store: Arc<Store>,
    session_id: String,
    mut rx: tokio::sync::mpsc::Receiver<FileTerminal>,
) {
    let mut buf: Vec<FileTerminal> = Vec::with_capacity(500);
    loop {
        buf.clear();
        match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
            Ok(Some(r)) => buf.push(r),
            Ok(None) => break,  // 发送侧全 drop（ctx 销毁）
            Err(_) => continue, // 聚合窗内无记录
        }
        while buf.len() < 500 {
            match rx.try_recv() {
                Ok(r) => buf.push(r),
                Err(_) => break,
            }
        }
        flush_history(&store, &session_id, &mut buf).await;
    }
    // 关闭前排空残留
    while let Ok(r) = rx.try_recv() {
        buf.push(r);
    }
    if !buf.is_empty() {
        flush_history(&store, &session_id, &mut buf).await;
    }
}

async fn flush_history(store: &Arc<Store>, session_id: &str, buf: &mut Vec<FileTerminal>) {
    for rec in buf.drain(..) {
        let _ = store
            .transfers()
            .upsert(&core_store::TransferRecord {
                id: rec.id,
                session_id: session_id.to_string(),
                direction: match rec.direction {
                    TransferDirection::Upload => "upload".into(),
                    TransferDirection::Download => "download".into(),
                },
                local: rec.local.to_string_lossy().to_string(),
                remote: rec.remote,
                bytes_done: rec.bytes_done,
                bytes_total: rec.bytes_total,
                state: rec.state.as_str().into(),
                error: rec.error,
                updated_at: String::new(), // 写入侧由 SQLite 时钟生成
            })
            .await;
    }
}

/// job 快照 → IPC 投影（rate 由订阅侧差分注入）
fn job_to_json(j: &JobSnapshot, rate: u64) -> Value {
    json!({
        "id": j.id,
        "direction": match j.direction {
            TransferDirection::Upload => "upload",
            TransferDirection::Download => "download",
        },
        "summary": j.summary,
        "state": j.state.as_str(),
        "paused": j.paused,
        "scanDone": j.scan_done,
        "discoveredFiles": j.discovered_files,
        "discoveredBytes": j.discovered_bytes,
        "completedFiles": j.completed_files,
        "failedFiles": j.failed_files,
        "skipped": j.skipped,
        "bytesDone": j.bytes_done,
        "error": j.error,
        "current": j.current,
        "failedEntries": j.failed_entries.iter().map(|e| json!({
            "path": e.path,
            "error": e.error,
        })).collect::<Vec<_>>(),
        "rate": rate,
    })
}

pub(crate) fn transfer_to_json(t: &core_sftp::TransferInfo) -> Value {
    json!({
        "id": t.id,
        "direction": match t.direction {
            TransferDirection::Upload => "upload",
            TransferDirection::Download => "download",
        },
        "local": t.local.to_string_lossy(),
        "remote": t.remote,
        "state": t.state.as_str(),
        "bytesDone": t.bytes_done,
        "bytesTotal": t.bytes_total,
        "onExists": t.on_exists.as_str(),
        "retries": t.retries,
        "error": t.error,
    })
}

/// 解析 onExists 参数（缺省 resume，保持既有续传行为）
fn parse_on_exists(raw: Option<String>) -> Result<OnExists, String> {
    match raw.as_deref() {
        None | Some("resume") => Ok(OnExists::Resume),
        Some("overwrite") => Ok(OnExists::Overwrite),
        Some("skip") => Ok(OnExists::Skip),
        Some("rename") => Ok(OnExists::Rename),
        Some(other) => Err(format!(
            "未知 onExists 策略: {other}（仅支持 resume/overwrite/skip/rename）"
        )),
    }
}

/// 远端目标冲突解析：Ok(None) = skip；Ok(Some((最终路径, 运行期模式))) = 入队
async fn resolve_remote_target(
    ctx: &SftpCtx,
    target: &str,
    policy: OnExists,
) -> Result<Option<(String, OnExists)>, String> {
    // stat 探测是冲突解析的正常流程（失败=目标不存在，不记可疑）
    let mc = ctx.metadata.get().await.map_err(|e| e.to_string())?;
    if mc.stat(target).await.is_err() {
        // 不存在（或不可 stat）：直接入队，运行期续传逻辑自负盈亏
        return Ok(Some((target.to_string(), policy.runtime())));
    }
    match policy {
        OnExists::Resume | OnExists::Overwrite => Ok(Some((target.to_string(), policy))),
        OnExists::Skip => Ok(None),
        OnExists::Rename => {
            for n in 1..1000 {
                let cand = rename_candidate(target, n);
                if mc.stat(&cand).await.is_err() {
                    return Ok(Some((cand, OnExists::Resume)));
                }
            }
            Err(format!("自动改名失败: {target} 的 name-N 候选均被占用"))
        }
    }
}

/// 本地目标冲突解析（与远端同策略；存在性看 std::fs）
fn resolve_local_target(
    target: &Path,
    policy: OnExists,
) -> Result<Option<(PathBuf, OnExists)>, String> {
    if !target.exists() {
        return Ok(Some((target.to_path_buf(), policy.runtime())));
    }
    match policy {
        OnExists::Resume | OnExists::Overwrite => Ok(Some((target.to_path_buf(), policy))),
        OnExists::Skip => Ok(None),
        OnExists::Rename => {
            let s = target.to_string_lossy();
            for n in 1..1000 {
                let cand = rename_candidate(&s, n);
                if !Path::new(&cand).exists() {
                    return Ok(Some((PathBuf::from(cand), OnExists::Resume)));
                }
            }
            Err(format!("自动改名失败: {} 的 name-N 候选均被占用", s))
        }
    }
}

/// 上传：local 文件/目录 → remote 目标目录（remote 为目录路径，文件名取本地名）。
/// on_exists 冲突策略逐文件生效（目录任务内每个文件独立判定）。
/// 目录 → DirectoryJob（立即返回 jobId，PR-8）；单文件 → 旧队列逐条路径不变。
#[tauri::command]
pub async fn sftp_upload(
    session_id: String,
    local: String,
    remote: String,
    on_exists: Option<String>,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let policy = parse_on_exists(on_exists)?;
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue
        .set_progress_callback(persist_terminal(sessions.store.clone(), session_id.clone()));
    let local_path = PathBuf::from(&local);
    let meta = std::fs::metadata(&local_path).map_err(|e| format!("本地路径不可读: {e}"))?;
    let base_name = local_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed".into());
    if meta.is_dir() {
        let remote_root = format!("{remote}/{base_name}");
        let mc = ctx.metadata.get().await.map_err(|e| e.to_string())?;
        mc.mkdir(&remote_root)
            .await
            .or_else(|e| {
                if e.to_string().contains("Failure") {
                    Ok(())
                } else {
                    Err(e)
                }
            })
            .map_err(|e| e.to_string())?;
        let job_id = ctx.jobs.submit(JobSpec {
            direction: TransferDirection::Upload,
            roots: vec![JobRoot {
                local: local_path.clone(),
                remote: remote_root,
            }],
            policy,
        });
        audit(
            &sessions.store,
            &session_id,
            "sftp_upload",
            &format!("{local} -> {remote}（目录任务 {job_id}）"),
        )
        .await;
        return Ok(json!({ "job": true, "jobId": job_id, "skipped": 0 }));
    }
    let mut ids = Vec::new();
    let mut skipped = 0u32;
    if let Some((path, mode)) =
        resolve_remote_target(&ctx, &format!("{remote}/{base_name}"), policy).await?
    {
        ids.push(
            ctx.queue
                .enqueue_upload(local_path, path, meta.len(), mode)
                .await,
        );
    } else {
        skipped += 1;
    }
    audit(
        &sessions.store,
        &session_id,
        "sftp_upload",
        &format!(
            "{local} -> {remote}（{} 个任务，跳过 {skipped}）",
            ids.len()
        ),
    )
    .await;
    Ok(json!({ "transferIds": ids, "skipped": skipped }))
}

/// 下载：remote 文件/目录 → local 目标目录。
/// 目录 → DirectoryJob（立即返回 jobId，PR-8）；单文件 → 旧队列逐条路径不变。
#[tauri::command]
pub async fn sftp_download(
    session_id: String,
    remote: String,
    local: String,
    on_exists: Option<String>,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let policy = parse_on_exists(on_exists)?;
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue
        .set_progress_callback(persist_terminal(sessions.store.clone(), session_id.clone()));
    let st = meta(&ctx, |c| {
        let r = &remote;
        async move { c.stat(r).await }
    })
    .await?;
    let base_name = st.name.clone();
    let local_base = PathBuf::from(&local);
    if st.kind == EntryKind::Dir {
        let target = local_base.join(&base_name);
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        let job_id = ctx.jobs.submit(JobSpec {
            direction: TransferDirection::Download,
            roots: vec![JobRoot {
                local: target,
                remote: remote.clone(),
            }],
            policy,
        });
        audit(
            &sessions.store,
            &session_id,
            "sftp_download",
            &format!("{remote} -> {local}（目录任务 {job_id}）"),
        )
        .await;
        return Ok(json!({ "job": true, "jobId": job_id, "skipped": 0 }));
    }
    let mut ids = Vec::new();
    let mut skipped = 0u32;
    std::fs::create_dir_all(&local_base).map_err(|e| e.to_string())?;
    match resolve_local_target(&local_base.join(&base_name), policy)? {
        Some((path, mode)) => {
            ids.push(
                ctx.queue
                    .enqueue_download(remote.clone(), path, st.size, mode)
                    .await,
            );
        }
        None => skipped += 1,
    }
    audit(
        &sessions.store,
        &session_id,
        "sftp_download",
        &format!(
            "{remote} -> {local}（{} 个任务，跳过 {skipped}）",
            ids.len()
        ),
    )
    .await;
    Ok(json!({ "transferIds": ids, "skipped": skipped }))
}

// ---------- DirectoryJob 命令族（PR-8） ----------

/// 目录任务列表（TransferCenter Job 区 / 调试）
#[tauri::command]
pub async fn transfer_job_list(
    session_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let jobs: Vec<Value> = ctx.jobs.list().iter().map(|j| job_to_json(j, 0)).collect();
    Ok(json!({ "jobs": jobs }))
}

#[tauri::command]
pub async fn transfer_job_pause(
    session_id: String,
    job_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.jobs.pause(&job_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn transfer_job_resume(
    session_id: String,
    job_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.jobs.resume(&job_id).map_err(|e| e.to_string())
}

/// 取消目录任务：停止发现、丢弃未开始文件、在途 chunk 边界中断
#[tauri::command]
pub async fn transfer_job_cancel(
    session_id: String,
    job_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.jobs.cancel(&job_id).map_err(|e| e.to_string())
}

/// 重试终态目录任务：Resume 策略重扫重跑（已完成文件秒级短路），返回新 jobId
#[tauri::command]
pub async fn transfer_job_retry(
    session_id: String,
    job_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let new_id = ctx.jobs.retry(&job_id).map_err(|e| e.to_string())?;
    Ok(json!({ "jobId": new_id }))
}

/// 移除终态目录任务（进行中拒绝）
#[tauri::command]
pub async fn transfer_job_remove(
    session_id: String,
    job_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.jobs.remove(&job_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn transfer_list(
    session_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let mut live: Vec<Value> = ctx.queue.list().iter().map(transfer_to_json).collect();
    // 历史（上次会话的终态记录）合并：live 已有的 id 以 live 为准
    let history = sessions
        .store
        .transfers()
        .for_session(&session_id)
        .await
        .map_err(|e| e.to_string())?;
    let live_ids: std::collections::HashSet<String> = live
        .iter()
        .filter_map(|v| v["id"].as_str().map(String::from))
        .collect();
    for h in history {
        if !live_ids.contains(&h.id) {
            live.push(json!({
                "id": h.id,
                "direction": h.direction,
                "local": h.local,
                "remote": h.remote,
                "state": h.state,
                "bytesDone": h.bytes_done,
                "bytesTotal": h.bytes_total,
                "retries": 0,
                "error": h.error,
                "history": true,
            }));
        }
    }
    Ok(json!({ "transfers": live }))
}

/// 全部会话的持久化传输历史（transfers 表，含时间；TransferCenter 历史记录区）
#[tauri::command]
pub async fn transfer_history(
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let records = sessions
        .store
        .transfers()
        .recent(200)
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({ "records": records }))
}

/// 清空全部传输历史记录
#[tauri::command]
pub async fn transfer_history_clear(
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<u64, String> {
    sessions
        .store
        .transfers()
        .clear_all()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn transfer_pause(
    session_id: String,
    transfer_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue.pause(&transfer_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn transfer_resume(
    session_id: String,
    transfer_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue.resume(&transfer_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn transfer_cancel(
    session_id: String,
    transfer_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue.cancel(&transfer_id).map_err(|e| e.to_string())
}
/// 重试失败/已取消的传输（断点续传自动沿用，无需前端传偏移）
#[tauri::command]
pub async fn transfer_retry(
    session_id: String,
    transfer_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue.retry(&transfer_id).map_err(|e| e.to_string())
}

/// 移除单条终态传输记录（进行中拒绝）
#[tauri::command]
pub async fn transfer_remove(
    session_id: String,
    transfer_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue.remove(&transfer_id).map_err(|e| e.to_string())
}

/// 批量清理终态传输：filter ∈ "done"|"failed"，返回移除数
#[tauri::command]
pub async fn transfer_clear(
    session_id: String,
    filter: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<u32, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    match filter.as_str() {
        "done" => Ok(ctx
            .queue
            .clear_where(|s| s == core_sftp::TransferState::Done)),
        "failed" => Ok(ctx
            .queue
            .clear_where(|s| s == core_sftp::TransferState::Failed)),
        _ => Err(format!("未知清理过滤: {filter}（仅支持 done/failed）")),
    }
}

#[tauri::command]
pub async fn transfer_pause_all(
    session_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue.pause_all();
    Ok(())
}

#[tauri::command]
pub async fn transfer_resume_all(
    session_id: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    ctx.queue.resume_all();
    Ok(())
}

// ---------- PR-9 增量事件协议 ----------

/// 单帧事件数上界（PR-9 帧边界）
const MAX_EVENTS_PER_FRAME: usize = 256;
/// 单帧序列化字节上界（PR-9 帧边界）
const MAX_FRAME_BYTES: usize = 256 * 1024;
/// 心跳间隔（无变化帧时保活；前端看门狗判活重建）
const HEARTBEAT_TICKS: u32 = 10; // 10 × 500ms = 5s

/// 事件信封（PR-9）：id=transferId/jobId；eventSeq 在 id 内按代际严格递增。
/// upsert 载荷为全量实体状态——乱序/重复/丢失均可收敛（C3：投影绝不反压传输）。
#[derive(Debug, Clone, PartialEq)]
struct TransferEvent {
    id: String,
    kind: &'static str,       // "transfer" | "job"
    event_type: &'static str, // "upsert" | "remove"
    seq: u64,
    payload: Value, // remove 时为 Null
}

fn event_json(generation: u64, e: &TransferEvent) -> Value {
    json!({
        "id": e.id,
        "kind": e.kind,
        "eventType": e.event_type,
        "generation": generation,
        "eventSeq": e.seq,
        "payload": e.payload,
    })
}

/// 帧切分（PR-9 帧边界）：≤256 事件且序列化 ≤256KB，超出拆帧
fn chunk_frames(frame_type: &str, generation: u64, events: &[TransferEvent]) -> Vec<Value> {
    let mut frames = Vec::new();
    let mut cur: Vec<Value> = Vec::new();
    let mut bytes = 64usize; // 信封余量
    for e in events {
        let v = event_json(generation, e);
        let len = v.to_string().len() + 1;
        if !cur.is_empty() && (cur.len() >= MAX_EVENTS_PER_FRAME || bytes + len > MAX_FRAME_BYTES) {
            frames.push(json!({ "type": frame_type, "generation": generation, "events": cur }));
            cur = Vec::new();
            bytes = 64;
        }
        bytes += len;
        cur.push(v);
    }
    if !cur.is_empty() {
        frames.push(json!({ "type": frame_type, "generation": generation, "events": cur }));
    }
    frames
}

/// 进度订阅（PR-9 增量事件协议 + snapshot 重同步）：
/// 首帧 snapshot（每实体当前 eventSeq），之后 500ms tick-diff 只发变化的 upsert/remove；
/// upsert 全量载荷（进度槽语义：最新值可覆盖）；无变化 5s 心跳；Channel 失败即弃订阅
/// （C3：IPC 是可重建投影，绝不反压 SFTP 数据面）。前端序号缺口/心跳超时 → 拆订阅重建。
#[tauri::command]
pub async fn transfer_subscribe(
    session_id: String,
    events: Channel<Value>,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let queue = ctx.queue.clone();
    let jobs = ctx.jobs.clone();
    static GENERATION_SEQ: AtomicU64 = AtomicU64::new(1);
    tauri::async_runtime::spawn(async move {
        let generation = GENERATION_SEQ.fetch_add(1, Ordering::Relaxed);
        // 本代际 per-id 序号簿（代际间唯一、代际内严格递增）
        let mut seqs: HashMap<String, u64> = HashMap::new();
        let next_seq = |seqs: &mut HashMap<String, u64>, id: &str| {
            let s = seqs.entry(id.to_string()).or_insert(0);
            *s += 1;
            *s
        };
        // 速率差分簿与变化指纹簿
        let mut rate_book: HashMap<String, (Instant, u64)> = HashMap::new();
        let mut prev: HashMap<String, String> = HashMap::new();
        let rate_of = |rate_book: &mut HashMap<String, (Instant, u64)>,
                       key: &str,
                       now: Instant,
                       bytes: u64| {
            let r = rate_book
                .get(key)
                .map(|(t0, b0)| {
                    let dt = now.duration_since(*t0).as_secs_f64();
                    if dt > 0.0 {
                        (bytes.saturating_sub(*b0) as f64 / dt) as u64
                    } else {
                        0
                    }
                })
                .unwrap_or(0);
            rate_book.insert(key.to_string(), (now, bytes));
            r
        };
        // 当前全量实体（transfer 键 "t:<id>"，job 键 "j:<id>"）
        let collect = |rate_book: &mut HashMap<String, (Instant, u64)>| {
            let now = Instant::now();
            let mut cur: Vec<(String, &'static str, Value)> = Vec::new();
            for t in queue.list() {
                let rate = rate_of(rate_book, &format!("t:{}", t.id), now, t.bytes_done);
                let mut v = transfer_to_json(&t);
                v["rate"] = json!(rate);
                cur.push((format!("t:{}", t.id), "transfer", v));
            }
            for j in jobs.list() {
                let key = format!("j:{}", j.id);
                let rate = rate_of(rate_book, &key, now, j.bytes_done);
                cur.push((key, "job", job_to_json(&j, rate)));
            }
            cur
        };
        // 初始 snapshot：每实体分配首个 eventSeq（= 该代际 snapshotSeq 语义）
        let snapshot: Vec<TransferEvent> = collect(&mut rate_book)
            .into_iter()
            .map(|(key, kind, payload)| {
                prev.insert(key.clone(), payload.to_string());
                TransferEvent {
                    seq: next_seq(&mut seqs, &key),
                    id: key,
                    kind,
                    event_type: "upsert",
                    payload,
                }
            })
            .collect();
        for frame in chunk_frames("snapshot", generation, &snapshot) {
            if events.send(frame).is_err() {
                return;
            }
        }
        let mut idle = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let cur = collect(&mut rate_book);
            let mut cur_keys = std::collections::HashSet::with_capacity(cur.len());
            let mut batch: Vec<TransferEvent> = Vec::new();
            for (key, kind, payload) in cur {
                cur_keys.insert(key.clone());
                let fp = payload.to_string();
                if prev.get(&key) == Some(&fp) {
                    continue; // 无变化：进度槽天然合并（只发最新值）
                }
                prev.insert(key.clone(), fp);
                batch.push(TransferEvent {
                    seq: next_seq(&mut seqs, &key),
                    id: key,
                    kind,
                    event_type: "upsert",
                    payload,
                });
            }
            // 消失的实体 → remove（唯一不可丢语义；序号缺口由前端重建兜底）
            let gone: Vec<String> = prev
                .keys()
                .filter(|k| !cur_keys.contains(*k))
                .cloned()
                .collect();
            for key in gone {
                prev.remove(&key);
                let kind = if key.starts_with("j:") {
                    "job"
                } else {
                    "transfer"
                };
                batch.push(TransferEvent {
                    seq: next_seq(&mut seqs, &key),
                    id: key,
                    kind,
                    event_type: "remove",
                    payload: Value::Null,
                });
            }
            if batch.is_empty() {
                idle += 1;
                if idle >= HEARTBEAT_TICKS {
                    idle = 0;
                    if events
                        .send(json!({ "type": "heartbeat", "generation": generation }))
                        .is_err()
                    {
                        return;
                    }
                }
                continue;
            }
            idle = 0;
            for frame in chunk_frames("events", generation, &batch) {
                if events.send(frame).is_err() {
                    return; // 前端关闭订阅/Channel 死亡——投影终止，不反压数据面
                }
            }
        }
    });
    Ok(())
}

// ---------- 远程直编 ----------

/// 下载远端文件到本地临时区并返回路径；后台 1s 轮询 mtime，变更即回传。
/// 编辑器生命周期外无法可靠感知 → 监视直到 app 退出（编辑场景文件小，轮询开销可忽略）。
#[tauri::command]
pub async fn sftp_edit_open(
    session_id: String,
    remote: String,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<Value, String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let st = meta(&ctx, |c| {
        let r = &remote;
        async move { c.stat(r).await }
    })
    .await?;
    if st.kind == EntryKind::Dir {
        return Err("不能编辑目录".into());
    }
    let seq = EDIT_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("myssh-edit-{seq}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let local = dir.join(&st.name);
    // 下载（同步等待完成——编辑前置动作，用户感知为打开耗时）
    // 临时区目录唯一（myssh-edit-<seq>），目标必不存在 → 策略无冲突，传 Resume
    let id = ctx
        .queue
        .enqueue_download(remote.clone(), local.clone(), st.size, OnExists::Resume)
        .await;
    let deadline = Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let info = ctx.queue.get(&id).ok_or("传输丢失")?;
        match info.state {
            core_sftp::TransferState::Done => break,
            core_sftp::TransferState::Failed | core_sftp::TransferState::Canceled => {
                return Err(info.error.unwrap_or_else(|| "下载失败".into()));
            }
            _ => {}
        }
        if Instant::now() > deadline {
            return Err("编辑下载超时".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    // 长生命周期监视回路持当前代际 client；死亡后下次写入失败由 meta 路径重建
    let client = ctx.metadata.get().await.map_err(|e| e.to_string())?;
    let local_w = local.clone();
    let remote_w = remote.clone();
    let store = sessions.store.clone();
    let sid = session_id.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_mtime = std::fs::metadata(&local_w).and_then(|m| m.modified()).ok();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let mtime = std::fs::metadata(&local_w).and_then(|m| m.modified()).ok();
            if mtime.is_none() {
                break; // 临时文件被删 → 结束监视
            }
            if mtime != last_mtime {
                last_mtime = mtime;
                let data = match std::fs::read(&local_w) {
                    Ok(d) => d,
                    Err(_) => continue, // 编辑器持锁瞬间，下轮再试
                };
                // 整文件覆盖：显式定长，新内容短时尾部不留旧字节
                if client.overwrite(&remote_w, &data).await.is_ok() {
                    audit(&store, &sid, "sftp_edit_save", &remote_w).await;
                }
            }
        }
    });
    audit(&sessions.store, &session_id, "sftp_edit_open", &remote).await;
    Ok(json!({ "localPath": local.to_string_lossy() }))
}

// ---------- 内部 ----------

pub(crate) async fn audit(store: &Arc<Store>, session_id: &str, action: &str, detail: &str) {
    let _ = store
        .audit()
        .append(
            core_store::Actor::Gui,
            Some(session_id),
            action,
            &json!({ "detail": detail }),
        )
        .await;
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn ev(id: &str, payload_len: usize) -> TransferEvent {
        TransferEvent {
            id: id.to_string(),
            kind: "transfer",
            event_type: "upsert",
            seq: 1,
            payload: json!({ "data": "x".repeat(payload_len) }),
        }
    }

    /// PR-9 帧边界：>256 事件拆帧、单帧 ≤256；大载荷按 256KB 拆帧
    #[test]
    fn chunk_frames_respects_count_and_byte_bounds() {
        // 数量边界：600 事件 → 3 帧（256+256+88），不越界
        let events: Vec<TransferEvent> = (0..600).map(|i| ev(&format!("t:{i}"), 8)).collect();
        let frames = chunk_frames("events", 7, &events);
        assert_eq!(frames.len(), 3);
        assert_eq!(
            frames[0]["events"].as_array().unwrap().len(),
            MAX_EVENTS_PER_FRAME
        );
        assert_eq!(
            frames[1]["events"].as_array().unwrap().len(),
            MAX_EVENTS_PER_FRAME
        );
        assert_eq!(frames[2]["events"].as_array().unwrap().len(), 88);
        assert!(frames
            .iter()
            .all(|f| f.to_string().len() <= MAX_FRAME_BYTES + 1024));
        // 字节边界：3 个 120KB 载荷 → 2 帧（两枚 ~240KB 恰好同帧，第三枚拆帧）
        let big: Vec<TransferEvent> = (0..3).map(|i| ev(&format!("t:{i}"), 120 * 1024)).collect();
        let frames = chunk_frames("events", 7, &big);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["events"].as_array().unwrap().len(), 2);
        // 空事件 → 零帧
        assert!(chunk_frames("events", 7, &[]).is_empty());
    }
}
