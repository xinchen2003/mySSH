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
    rename_candidate, DirEntry, EntryKind, OnExists, SftpClient, TransferDirection, TransferQueue,
};
use core_store::Store;

use crate::sessions::SessionManagerState;

static EDIT_SEQ: AtomicU64 = AtomicU64::new(1);

/// 单会话的 SFTP 上下文（连接 + 客户端 + 传输队列）
pub struct SftpCtx {
    /// 保活：conn 在则通道在
    _conn: core_ssh::SshConnection,
    client: Arc<SftpClient>,
    queue: Arc<TransferQueue>,
}

pub struct SftpManagerState {
    ctxs: Mutex<HashMap<String, Arc<SftpCtx>>>,
    rt: tokio::runtime::Handle,
}

impl SftpCtx {
    /// 监控复用同一 Bulk 连接（通道复用，不占交互连接）
    pub(crate) fn conn(&self) -> &core_ssh::SshConnection {
        &self._conn
    }
    /// MCP SFTP 工具复用（与 UI 共享 Bulk 连接池，避免 agent 高频小操作反复握手）
    pub(crate) fn client(&self) -> &Arc<SftpClient> {
        &self.client
    }

    /// MCP 传输工具复用（与 UI 同一队列：进度进 UI 传输面板、终态落 transfers 表）
    pub(crate) fn queue(&self) -> &Arc<TransferQueue> {
        &self.queue
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
            .map(|(id, ctx)| {
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
                    "tasks": tasks.len(),
                    "queued": queued,
                    "running": running,
                    "paused": paused,
                })
            })
            .collect();
        json!({ "contexts": ctxs.len(), "sessions": sessions })
    }
}

/// 取/建会话的 SFTP 上下文（Bulk 连接 + SFTP 子系统通道，bulk-rt 上建立）
pub(crate) async fn ensure_ctx(
    state: &Arc<SftpManagerState>,
    store: &Arc<Store>,
    session_id: &str,
) -> Result<Arc<SftpCtx>, String> {
    if let Some(ctx) = state.ctxs.lock().get(session_id) {
        if !ctx._conn.is_closed() {
            return Ok(ctx.clone());
        }
        state.ctxs.lock().remove(session_id); // 死连接剔除，重建
    }
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
    let rt = state.rt.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    rt.spawn(async move {
        let result = async {
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
            let client = Arc::new(SftpClient::open(&conn).await.map_err(|e| e.to_string())?);
            let queue = Arc::new(TransferQueue::new(
                client.clone(),
                3,
                tokio::runtime::Handle::current(),
            ));
            Ok::<_, String>(SftpCtx {
                _conn: conn,
                client,
                queue,
            })
        }
        .await;
        let _ = tx.send(result);
    });
    let ctx = Arc::new(rx.await.map_err(|_| "bulk-rt 连接任务丢失".to_string())??);
    // 并发建连去重：等待 rx 期间可能有别的调用已建好并插入（SFTP 打开瞬间
    // sftp_list / transfer_list / transfer_subscribe 并发触发）。若不检查，
    // 后插入者覆盖 map，而先返回的调用方（如 transfer_subscribe 推送循环）
    // 永久持有孤儿 ctx 的空 queue —— 订阅帧恒空、前端传输面板无任何反馈。
    let mut map = state.ctxs.lock();
    if let Some(existing) = map.get(session_id) {
        if !existing._conn.is_closed() {
            return Ok(existing.clone()); // 多余的自建 ctx 随 drop 关闭连接
        }
    }
    map.insert(session_id.to_string(), ctx.clone());
    drop(map);
    Ok(ctx)
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
    let entries = ctx.client.list(&path).await.map_err(|e| e.to_string())?;
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
    let e = ctx.client.stat(&path).await.map_err(|e| e.to_string())?;
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
    ctx.client.mkdir(&path).await.map_err(|e| e.to_string())?;
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
    ctx.client
        .remove_recursive(&path)
        .await
        .map_err(|e| e.to_string())?;
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
    ctx.client
        .rename(&from, &to)
        .await
        .map_err(|e| e.to_string())?;
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
    ctx.client
        .chmod(&path, mode)
        .await
        .map_err(|e| e.to_string())?;
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
    if ctx.client.stat(&path).await.is_ok() {
        return Err(format!("目标已存在: {path}"));
    }
    use tokio::io::AsyncWriteExt;
    let mut f = ctx
        .client
        .open_write_at(&path, 0)
        .await
        .map_err(|e| e.to_string())?;
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
    resolve_home_abs(&ctx.client).await
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
    let home = resolve_home_abs(&ctx.client).await?;
    let bash = read_rc_file(&ctx.client, &format!("{home}/.bashrc")).await?;
    let zsh = read_rc_file(&ctx.client, &format!("{home}/.zshrc")).await?;
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
    let home = resolve_home_abs(&ctx.client).await?;
    let mut targets = vec![format!("{home}/.bashrc")];
    let zshrc = format!("{home}/.zshrc");
    if ctx.client.lstat(&zshrc).await.is_ok() {
        targets.push(zshrc);
    }
    let mut touched: Vec<String> = Vec::new();
    for path in &targets {
        let content = read_rc_file(&ctx.client, path).await?.unwrap_or_default();
        let next = if enable {
            core_sftp::add_integration(&content)
        } else {
            core_sftp::remove_integration(&content)
        };
        if next != content {
            ctx.client
                .overwrite(path, next.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            touched.push(path.clone());
        }
    }
    // 脚本本体 ~/.myssh/osc7.sh：enable 写入（目录不在则建），disable 删除。
    // rc 块与终端激活行都只是 source 它——用户在 rc/终端里看到的只有一行。
    let script = format!("{home}/{}", core_sftp::SCRIPT_REL);
    if enable {
        let dir = format!("{home}/.myssh");
        if ctx.client.lstat(&dir).await.is_err() {
            ctx.client.mkdir(&dir).await.map_err(|e| e.to_string())?;
        }
        ctx.client
            .overwrite(&script, core_sftp::SCRIPT.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        touched.push(script);
    } else if ctx.client.lstat(&script).await.is_ok() {
        ctx.client
            .remove_file(&script)
            .await
            .map_err(|e| e.to_string())?;
        // 目录空则顺带收掉；非空（用户自己放了东西）remove_dir 会失败，忽略
        let _ = ctx.client.remove_dir(&format!("{home}/.myssh")).await;
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
    if ctx.client.stat(target).await.is_err() {
        // 不存在（或不可 stat）：直接入队，运行期续传逻辑自负盈亏
        return Ok(Some((target.to_string(), policy.runtime())));
    }
    match policy {
        OnExists::Resume | OnExists::Overwrite => Ok(Some((target.to_string(), policy))),
        OnExists::Skip => Ok(None),
        OnExists::Rename => {
            for n in 1..1000 {
                let cand = rename_candidate(target, n);
                if ctx.client.stat(&cand).await.is_err() {
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
/// on_exists 冲突策略逐文件生效（目录递归展开后每个文件独立判定），返回 skipped 计数。
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
    let mut ids = Vec::new();
    let mut skipped = 0u32;
    if meta.is_dir() {
        // 递归展开：远端 mkdir 链 + 逐文件入队
        enqueue_dir_upload(
            &ctx,
            &local_path,
            &format!("{remote}/{base_name}"),
            policy,
            &mut ids,
            &mut skipped,
        )
        .await?;
    } else if let Some((path, mode)) =
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

async fn enqueue_dir_upload(
    ctx: &SftpCtx,
    local_dir: &Path,
    remote_dir: &str,
    policy: OnExists,
    ids: &mut Vec<String>,
    skipped: &mut u32,
) -> Result<(), String> {
    ctx.client
        .mkdir(remote_dir)
        .await
        .or_else(|e| {
            if e.to_string().contains("Failure") {
                Ok(()) // 已存在 → 忽略（mkdir 幂等近似）
            } else {
                Err(e)
            }
        })
        .map_err(|e| e.to_string())?;
    let rd = std::fs::read_dir(local_dir).map_err(|e| e.to_string())?;
    for e in rd {
        let e = e.map_err(|e| e.to_string())?;
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        let remote = format!("{remote_dir}/{name}");
        let meta = e.metadata().map_err(|e| e.to_string())?;
        if meta.is_dir() {
            Box::pin(enqueue_dir_upload(ctx, &p, &remote, policy, ids, skipped)).await?;
        } else if meta.is_file() {
            match resolve_remote_target(ctx, &remote, policy).await? {
                Some((path, mode)) => {
                    ids.push(ctx.queue.enqueue_upload(p, path, meta.len(), mode).await);
                }
                None => *skipped += 1,
            }
        }
    }
    Ok(())
}

/// 下载：remote 文件/目录 → local 目标目录。
/// on_exists 冲突策略逐文件生效（目录递归展开后每个文件独立判定），返回 skipped 计数。
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
    let st = ctx.client.stat(&remote).await.map_err(|e| e.to_string())?;
    let base_name = st.name.clone();
    let local_base = PathBuf::from(&local);
    let mut ids = Vec::new();
    let mut skipped = 0u32;
    if st.kind == EntryKind::Dir {
        let target = local_base.join(&base_name);
        std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
        Box::pin(enqueue_dir_download(
            &ctx,
            &remote,
            &target,
            policy,
            &mut ids,
            &mut skipped,
        ))
        .await?;
    } else {
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

async fn enqueue_dir_download(
    ctx: &SftpCtx,
    remote_dir: &str,
    local_dir: &Path,
    policy: OnExists,
    ids: &mut Vec<String>,
    skipped: &mut u32,
) -> Result<(), String> {
    let entries = ctx
        .client
        .list(remote_dir)
        .await
        .map_err(|e| e.to_string())?;
    for e in entries {
        let local = local_dir.join(&e.name);
        match e.kind {
            EntryKind::Dir => {
                std::fs::create_dir_all(&local).map_err(|e| e.to_string())?;
                Box::pin(enqueue_dir_download(
                    ctx, &e.path, &local, policy, ids, skipped,
                ))
                .await?;
            }
            EntryKind::File => match resolve_local_target(&local, policy)? {
                Some((path, mode)) => {
                    ids.push(ctx.queue.enqueue_download(e.path, path, e.size, mode).await);
                }
                None => *skipped += 1,
            },
            // 软链接/其他：跳过（规格书「软链接识别」= 不盲目跟随）
            _ => {}
        }
    }
    Ok(())
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

/// 进度订阅：500ms 快照推送（前端差分算速率；与 tunnel_subscribe 同构）
#[tauri::command]
pub async fn transfer_subscribe(
    session_id: String,
    events: Channel<Value>,
    state: tauri::State<'_, Arc<SftpManagerState>>,
    sessions: tauri::State<'_, Arc<SessionManagerState>>,
) -> Result<(), String> {
    let ctx = ensure_ctx(&state, &sessions.store, &session_id).await?;
    let queue = ctx.queue.clone();
    tauri::async_runtime::spawn(async move {
        let mut last: HashMap<String, (Instant, u64)> = HashMap::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let now = Instant::now();
            let frames: Vec<Value> = queue
                .list()
                .iter()
                .map(|t| {
                    let mut v = transfer_to_json(t);
                    // 速率差分
                    let rate = last
                        .get(&t.id)
                        .map(|(t0, b0)| {
                            let dt = now.duration_since(*t0).as_secs_f64();
                            if dt > 0.0 {
                                ((t.bytes_done - b0) as f64 / dt) as u64
                            } else {
                                0
                            }
                        })
                        .unwrap_or(0);
                    last.insert(t.id.clone(), (now, t.bytes_done));
                    v["rate"] = json!(rate);
                    v
                })
                .collect();
            if events.send(json!({ "transfers": frames })).is_err() {
                break; // 前端关闭订阅
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
    let st = ctx.client.stat(&remote).await.map_err(|e| e.to_string())?;
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

    // 回传监视
    let client = ctx.client.clone();
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
