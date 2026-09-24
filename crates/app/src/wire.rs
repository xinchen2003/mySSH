//! IPC 线型：跨进程帧/视图的单一表示（架构 review 卡 7，ADR-0008）。
//!
//! 这批形状此前是命令里的 `json!` 字面量 + `types.ts` 手抄镜像（漂移只在运行时炸，
//! 例：core TunnelInfo.session_id 上线后 json! 视图与 TS 双双落后）。现在 Rust 类型
//! 即契约，TS bindings 由 ts-rs 生成（`app/ui/src/term/bindings/`，`cargo test` 时刷新，
//! 文件入库——diff 即漂移）。serde 形状与既有线面逐字节一致：
//! `skip_serializing_if` 复刻「键缺省」、无 skip 的 Option 复刻「键在值 null」。
//! 数值一律 `#[ts(type = "number")]`——serde_json 运行时就是 number（ts-rs 默认
//! 给 u64 bigint 会撒谎）。窄域 String 用 `#[ts(type = "'a' | 'b'")]` 表达真实值域。

use serde::Serialize;
use ts_rs::TS;

/// 线型 → JSON Value。这批类型的序列化不会失败（无自定义 serializer）；
/// 失败归 Null 也绝不 panic（lint 禁 unwrap/expect）。
pub fn json_of<T: Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

// ---------- 隧道 ----------

/// 运行中隧道视图（tunnel_subscribe 帧元素；TS 名 TunnelInfo）。
/// sessionId：卡 6 起运行条目自持会话归属——此前线面缺这字段，前端只能靠定义反查归属。
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(
    export,
    export_to = "../../../app/ui/src/term/bindings/",
    rename = "TunnelInfo"
)]
pub struct TunnelView {
    pub tunnel_id: String,
    #[ts(type = "'local' | 'remote' | 'dynamic'")]
    pub kind: String,
    pub bind: String,
    pub target: Option<String>,
    #[ts(type = "'starting' | 'listening' | 'reconnecting' | 'stopped' | 'failed'")]
    pub status: String,
    #[ts(type = "number")]
    pub active_conns: u64,
    #[ts(type = "number")]
    pub total_conns: u64,
    #[ts(type = "number")]
    pub bytes_up: u64,
    #[ts(type = "number")]
    pub bytes_down: u64,
    #[ts(type = "number")]
    pub rate_up: u64,
    #[ts(type = "number")]
    pub rate_down: u64,
    #[ts(type = "number")]
    pub errors: u64,
    #[ts(type = "number")]
    pub rejected_conns: u64,
    pub reconnects: u32,
    pub last_error: Option<String>,
    /// 归属会话（空串 = 独立隧道，不参与会话失效扇出）
    pub session_id: String,
}

impl TunnelView {
    pub fn from_info(t: &core_tunnel::TunnelInfo, rate_up: u64, rate_down: u64) -> Self {
        Self {
            tunnel_id: t.id.clone(),
            kind: t.kind.clone(),
            bind: t.bind.clone(),
            target: t.target.clone(),
            status: match t.status {
                core_tunnel::TunnelStatus::Starting => "starting",
                core_tunnel::TunnelStatus::Listening => "listening",
                core_tunnel::TunnelStatus::Reconnecting => "reconnecting",
                core_tunnel::TunnelStatus::Stopped => "stopped",
                core_tunnel::TunnelStatus::Failed => "failed",
            }
            .into(),
            active_conns: t.stats.active_conns,
            total_conns: t.stats.total_conns,
            bytes_up: t.stats.bytes_up,
            bytes_down: t.stats.bytes_down,
            rate_up,
            rate_down,
            errors: t.stats.errors,
            rejected_conns: t.stats.rejected_conns,
            reconnects: t.stats.reconnects,
            last_error: t.last_error.clone(),
            session_id: t.session_id.clone(),
        }
    }
}

/// §9.6 随会话隧道启动结果（session_tunnels 帧 results[]）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct SessionTunnelResult {
    pub id: String,
    pub name: String,
    pub bind: String,
    pub ok: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct SessionTunnelsFrame {
    #[ts(type = "1")]
    pub v: u8,
    #[serde(rename = "type")]
    #[ts(type = "'session_tunnels'")]
    pub frame_type: String,
    pub session_id: String,
    pub results: Vec<SessionTunnelResult>,
}

impl SessionTunnelsFrame {
    pub fn new(session_id: &str, results: Vec<SessionTunnelResult>) -> Self {
        Self {
            v: 1,
            frame_type: "session_tunnels".into(),
            session_id: session_id.into(),
            results,
        }
    }
}

// ---------- 传输 ----------

/// 单文件传输视图（transfer_subscribe/transfer_list 帧元素）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct TransferView {
    pub id: String,
    #[ts(type = "'upload' | 'download'")]
    pub direction: String,
    pub local: String,
    pub remote: String,
    #[ts(type = "'queued' | 'running' | 'paused' | 'done' | 'failed' | 'canceled'")]
    pub state: String,
    #[ts(type = "number")]
    pub bytes_done: u64,
    #[ts(type = "number")]
    pub bytes_total: u64,
    pub retries: u32,
    /// 历史回放行无此键
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub priority: Option<bool>,
    /// 历史回放行无此键
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[ts(type = "'resume' | 'overwrite' | 'skip' | 'rename'")]
    pub on_exists: Option<String>,
    pub error: Option<String>,
    /// 仅订阅帧注入（差分速率）；list 帧无此键
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[ts(type = "number")]
    pub rate: Option<u64>,
    /// true = 历史回放行（上次运行终态）
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub history: Option<bool>,
}

impl TransferView {
    pub fn from_info(t: &core_sftp::TransferInfo) -> Self {
        Self {
            id: t.id.clone(),
            direction: match t.direction {
                core_sftp::TransferDirection::Upload => "upload",
                core_sftp::TransferDirection::Download => "download",
            }
            .into(),
            local: t.local.to_string_lossy().into_owned(),
            remote: t.remote.clone(),
            state: t.state.as_str().into(),
            bytes_done: t.bytes_done,
            bytes_total: t.bytes_total,
            retries: t.retries,
            priority: Some(t.priority),
            on_exists: Some(t.on_exists.as_str().into()),
            error: t.error.clone(),
            rate: None,
            history: None,
        }
    }

    /// 历史回放行（transfers 表记录 → live 列表的补集；retries 恒 0、history 恒 true）
    pub fn from_record(h: &core_store::TransferRecord) -> Self {
        Self {
            id: h.id.clone(),
            direction: h.direction.clone(),
            local: h.local.clone(),
            remote: h.remote.clone(),
            state: h.state.clone(),
            bytes_done: h.bytes_done,
            bytes_total: h.bytes_total,
            retries: 0,
            priority: None,
            on_exists: None,
            error: h.error.clone(),
            rate: None,
            history: Some(true),
        }
    }
}

/// 目录任务失败条目（TransferJobView.failedEntries[]）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct FailedEntry {
    pub path: String,
    pub error: String,
}

/// 目录任务视图（transfer_subscribe 帧 jobs[] / transfer_job_list）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct TransferJobView {
    pub id: String,
    #[ts(type = "'upload' | 'download'")]
    pub direction: String,
    pub summary: String,
    #[ts(
        type = "'scanning' | 'transferring' | 'finalizing' | 'completed' | 'failed' | 'canceled'"
    )]
    pub state: String,
    pub paused: bool,
    pub scan_done: bool,
    #[ts(type = "number")]
    pub discovered_files: u64,
    #[ts(type = "number")]
    pub discovered_bytes: u64,
    #[ts(type = "number")]
    pub completed_files: u64,
    #[ts(type = "number")]
    pub failed_files: u64,
    #[ts(type = "number")]
    pub skipped: u64,
    #[ts(type = "number")]
    pub bytes_done: u64,
    pub error: Option<String>,
    pub current: Vec<String>,
    pub failed_entries: Vec<FailedEntry>,
    #[ts(type = "number")]
    pub rate: u64,
}

impl TransferJobView {
    pub fn from_snapshot(j: &core_sftp::JobSnapshot, rate: u64) -> Self {
        Self {
            id: j.id.clone(),
            direction: match j.direction {
                core_sftp::TransferDirection::Upload => "upload",
                core_sftp::TransferDirection::Download => "download",
            }
            .into(),
            summary: j.summary.clone(),
            state: j.state.as_str().into(),
            paused: j.paused,
            scan_done: j.scan_done,
            discovered_files: j.discovered_files,
            discovered_bytes: j.discovered_bytes,
            completed_files: j.completed_files,
            failed_files: j.failed_files,
            skipped: j.skipped,
            bytes_done: j.bytes_done,
            error: j.error.clone(),
            current: j.current.clone(),
            failed_entries: j
                .failed_entries
                .iter()
                .map(|e| FailedEntry {
                    path: e.path.clone(),
                    error: e.error.clone(),
                })
                .collect(),
            rate,
        }
    }
}

// ---------- 浏览 ----------

/// SFTP/本地目录条目（sftp_list/local_list 帧元素）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    #[ts(type = "'file' | 'dir' | 'symlink' | 'other'")]
    pub kind: String,
    #[ts(type = "number")]
    pub size: u64,
    /// 本地列表无此键
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub permissions: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[ts(type = "number")]
    pub mtime: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub group: Option<String>,
}

impl FileEntry {
    pub fn from_remote(e: &core_sftp::DirEntry) -> Self {
        Self {
            name: e.name.clone(),
            path: e.path.clone(),
            kind: match e.kind {
                core_sftp::EntryKind::File => "file",
                core_sftp::EntryKind::Dir => "dir",
                core_sftp::EntryKind::Symlink => "symlink",
                core_sftp::EntryKind::Other => "other",
            }
            .into(),
            size: e.size,
            mtime: e.mtime.map(u64::from),
            permissions: e.permissions,
            user: e.user.clone(),
            group: e.group.clone(),
        }
    }

    /// 本地条目（无 permissions/user/group 键）
    pub fn local(
        name: String,
        path: String,
        is_dir: bool,
        is_symlink: bool,
        size: u64,
        mtime: Option<u64>,
    ) -> Self {
        Self {
            name,
            path,
            kind: if is_dir {
                "dir"
            } else if is_symlink {
                "symlink"
            } else {
                "file"
            }
            .into(),
            size,
            permissions: None,
            mtime,
            user: None,
            group: None,
        }
    }

    /// 盘符枚举条目（C: 等；mtime 键缺省）
    pub fn drive(name: String, path: String) -> Self {
        Self {
            name,
            path,
            kind: "dir".into(),
            size: 0,
            permissions: None,
            mtime: None,
            user: None,
            group: None,
        }
    }
}

// ---------- 终端事件帧 ----------

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct HostKeyPromptFrame {
    #[ts(type = "1")]
    pub v: u8,
    #[serde(rename = "type")]
    #[ts(type = "'hostkey_prompt'")]
    pub frame_type: String,
    pub confirm_id: String,
    #[ts(type = "'unknown' | 'changed'")]
    pub kind: String,
    pub host: String,
    pub port: u16,
    pub key_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub old_fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub new_fingerprint: Option<String>,
}

impl HostKeyPromptFrame {
    pub fn unknown(
        confirm_id: &str,
        host: &str,
        port: u16,
        key_type: &str,
        fingerprint: &str,
    ) -> Self {
        Self {
            v: 1,
            frame_type: "hostkey_prompt".into(),
            confirm_id: confirm_id.into(),
            kind: "unknown".into(),
            host: host.into(),
            port,
            key_type: key_type.into(),
            fingerprint: Some(fingerprint.into()),
            old_fingerprint: None,
            new_fingerprint: None,
        }
    }

    pub fn changed(
        confirm_id: &str,
        host: &str,
        port: u16,
        key_type: &str,
        old_fingerprint: &str,
        new_fingerprint: &str,
    ) -> Self {
        Self {
            v: 1,
            frame_type: "hostkey_prompt".into(),
            confirm_id: confirm_id.into(),
            kind: "changed".into(),
            host: host.into(),
            port,
            key_type: key_type.into(),
            fingerprint: None,
            old_fingerprint: Some(old_fingerprint.into()),
            new_fingerprint: Some(new_fingerprint.into()),
        }
    }
}

/// KI 挑战的单条提示（KiChallengeFrame.prompts[]）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct KiPrompt {
    pub prompt: String,
    pub echo: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct KiChallengeFrame {
    #[ts(type = "1")]
    pub v: u8,
    #[serde(rename = "type")]
    #[ts(type = "'ki_challenge'")]
    pub frame_type: String,
    pub confirm_id: String,
    pub name: String,
    pub instruction: String,
    pub prompts: Vec<KiPrompt>,
}

impl KiChallengeFrame {
    pub fn new(confirm_id: &str, challenge: &core_ssh::KiChallenge) -> Self {
        Self {
            v: 1,
            frame_type: "ki_challenge".into(),
            confirm_id: confirm_id.into(),
            name: challenge.name.clone(),
            instruction: challenge.instruction.clone(),
            prompts: challenge
                .prompts
                .iter()
                .map(|p| KiPrompt {
                    prompt: p.prompt.clone(),
                    echo: p.echo,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct SessionStateFrame {
    #[ts(type = "1")]
    pub v: u8,
    #[serde(rename = "type")]
    #[ts(type = "'session_state'")]
    pub frame_type: String,
    pub tab_id: String,
    #[ts(type = "'connected' | 'closed' | 'reconnecting' | 'error'")]
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub attempt: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub reconnected: Option<bool>,
    /// local 会话连接成功帧：'local' + 实际启动的 shell 程序名
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    #[ts(type = "'local'")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub shell: Option<String>,
    /// 首次连接成功帧携带的远端标识
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub user: Option<String>,
}

impl SessionStateFrame {
    fn base(tab_id: &str, state: &str) -> Self {
        Self {
            v: 1,
            frame_type: "session_state".into(),
            tab_id: tab_id.into(),
            state: state.into(),
            message: None,
            attempt: None,
            reconnected: None,
            kind: None,
            shell: None,
            host: None,
            port: None,
            user: None,
        }
    }

    /// 首次连接成功（SSH：携带远端标识）
    pub fn connected(tab_id: &str, host: &str, port: u16, user: &str) -> Self {
        let mut f = Self::base(tab_id, "connected");
        f.host = Some(host.into());
        f.port = Some(port);
        f.user = Some(user.into());
        f
    }

    /// local 会话连接成功（携带实际启动的 shell）
    pub fn connected_local(tab_id: &str, shell: &str) -> Self {
        let mut f = Self::base(tab_id, "connected");
        f.kind = Some("local".into());
        f.shell = Some(shell.into());
        f
    }

    /// 断线重连成功（区别于首次连接）
    pub fn reconnected(tab_id: &str) -> Self {
        let mut f = Self::base(tab_id, "connected");
        f.reconnected = Some(true);
        f
    }

    pub fn reconnecting(tab_id: &str, attempt: u32) -> Self {
        let mut f = Self::base(tab_id, "reconnecting");
        f.attempt = Some(attempt);
        f
    }

    pub fn closed(tab_id: &str) -> Self {
        Self::base(tab_id, "closed")
    }
}

/// 登录宏被后端跳过的告知帧（v1 唯一 reason：su 手输密码，应答时机不可知）
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub struct MacroSkippedFrame {
    #[ts(type = "1")]
    pub v: u8,
    #[serde(rename = "type")]
    #[ts(type = "'macro_skipped'")]
    pub frame_type: String,
    pub tab_id: String,
    #[ts(type = "'suManualPassword'")]
    pub reason: String,
}

impl MacroSkippedFrame {
    pub fn su_manual_password(tab_id: &str) -> Self {
        Self {
            v: 1,
            frame_type: "macro_skipped".into(),
            tab_id: tab_id.into(),
            reason: "suManualPassword".into(),
        }
    }
}

// ---------- 监控 ----------

/// metrics_subscribe 推送帧
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase", tag = "kind")]
#[ts(export, export_to = "../../../app/ui/src/term/bindings/")]
pub enum MetricsEvent {
    Snapshot { data: core_monitor::MetricsSnapshot },
    Error { message: String, fatal: bool },
}
