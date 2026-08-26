//! SFTP 客户端：封装 russh-sftp 的高层文件操作。
//!
//! 连接来源：core-ssh 的 session 通道 + `request_subsystem("sftp")`。
//! 传输性能：russh-sftp 默认 max_packet_len 256KiB、并发写 8——不做二次调优，
//! 预算核对走 flood_bench 手法（见 10-risks）。

use core_ssh::SshConnection;
use russh_sftp::client::fs::File;
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, OpenFlags};

use crate::SftpError;

/// 目录条目类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// 目录条目（对 UI 的直接投影；POSIX 语义）
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub kind: EntryKind,
    pub size: u64,
    /// 八位权限位（低 12 位，含 setuid 等；None = 服务器未给）
    pub permissions: Option<u32>,
    pub mtime: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub user: Option<String>,
    pub group: Option<String>,
}

impl DirEntry {
    fn from_raw(e: russh_sftp::client::fs::DirEntry) -> Self {
        let m = e.metadata();
        let kind = if m.is_symlink() {
            EntryKind::Symlink
        } else if m.is_dir() {
            EntryKind::Dir
        } else if m.is_regular() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Self {
            name: e.file_name(),
            path: e.path(),
            kind,
            size: m.size.unwrap_or(0),
            permissions: m.permissions,
            mtime: m.mtime,
            uid: m.uid,
            gid: m.gid,
            user: m.user,
            group: m.group,
        }
    }
}

fn map_err(path: &str, e: russh_sftp::client::error::Error) -> SftpError {
    SftpError::RemotePath {
        path: path.to_string(),
        reason: e.to_string(),
    }
}

/// SFTP 会话（一个 SSH 通道一份；批量传输应挂在 Bulk 连接上，不占交互连接）
pub struct SftpClient {
    session: SftpSession,
}

impl SftpClient {
    /// 在既有 SSH 连接上开 SFTP 子系统通道
    pub async fn open(conn: &SshConnection) -> Result<Self, SftpError> {
        let ch = conn.open_session_channel().await?;
        ch.request_subsystem(true, "sftp")
            .await
            .map_err(|e| SftpError::Subsystem(e.to_string()))?;
        let session = SftpSession::new(ch.into_stream())
            .await
            .map_err(|e| SftpError::Subsystem(e.to_string()))?;
        Ok(Self { session })
    }

    /// 目录列表（一次全量；UI 侧做窗口化渲染与按需展开）
    pub async fn list(&self, path: &str) -> Result<Vec<DirEntry>, SftpError> {
        let rd = self
            .session
            .read_dir(path)
            .await
            .map_err(|e| map_err(path, e))?;
        let mut out: Vec<DirEntry> = rd.map(DirEntry::from_raw).collect();
        out.retain(|e| e.name != "." && e.name != "..");
        // 目录在前，同名按字典序（文件管理器惯例）
        out.sort_by(|a, b| {
            (a.kind != EntryKind::Dir)
                .cmp(&(b.kind != EntryKind::Dir))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(out)
    }

    /// 元数据（不跟随软链接——软链接识别按规格书要求看链接本身）
    pub async fn lstat(&self, path: &str) -> Result<DirEntry, SftpError> {
        let m = self
            .session
            .symlink_metadata(path)
            .await
            .map_err(|e| map_err(path, e))?;
        let name = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
        let kind = if m.is_symlink() {
            EntryKind::Symlink
        } else if m.is_dir() {
            EntryKind::Dir
        } else if m.is_regular() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Ok(DirEntry {
            name: name.to_string(),
            path: path.to_string(),
            kind,
            size: m.size.unwrap_or(0),
            permissions: m.permissions,
            mtime: m.mtime,
            uid: m.uid,
            gid: m.gid,
            user: m.user,
            group: m.group,
        })
    }

    /// 跟随软链接的元数据（传输前取真实大小用）
    pub async fn stat(&self, path: &str) -> Result<DirEntry, SftpError> {
        let m = self
            .session
            .metadata(path)
            .await
            .map_err(|e| map_err(path, e))?;
        let name = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or(path);
        Ok(DirEntry {
            name: name.to_string(),
            path: path.to_string(),
            kind: if m.is_dir() {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            size: m.size.unwrap_or(0),
            permissions: m.permissions,
            mtime: m.mtime,
            uid: m.uid,
            gid: m.gid,
            user: m.user,
            group: m.group,
        })
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), SftpError> {
        self.session
            .create_dir(path)
            .await
            .map_err(|e| map_err(path, e))
    }

    pub async fn remove_file(&self, path: &str) -> Result<(), SftpError> {
        self.session
            .remove_file(path)
            .await
            .map_err(|e| map_err(path, e))
    }

    pub async fn remove_dir(&self, path: &str) -> Result<(), SftpError> {
        self.session
            .remove_dir(path)
            .await
            .map_err(|e| map_err(path, e))
    }

    /// 递归删除（目录树自底向上；软链接按链接删除，不跟随）
    pub async fn remove_recursive(&self, path: &str) -> Result<(), SftpError> {
        let st = self.lstat(path).await?;
        match st.kind {
            EntryKind::Dir => {
                for e in self.list(path).await? {
                    Box::pin(self.remove_recursive(&e.path)).await?;
                }
                self.remove_dir(path).await
            }
            // Symlink/File/Other 都按文件删（remove_file 对软链接删链接本身）
            _ => self.remove_file(path).await,
        }
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), SftpError> {
        self.session
            .rename(from, to)
            .await
            .map_err(|e| map_err(from, e))
    }

    /// 修改权限（mode 为八进制位，如 0o644）
    pub async fn chmod(&self, path: &str, mode: u32) -> Result<(), SftpError> {
        let attrs = FileAttributes {
            permissions: Some(mode),
            ..Default::default()
        };
        self.session
            .set_metadata(path, attrs)
            .await
            .map_err(|e| map_err(path, e))
    }

    pub async fn read_link(&self, path: &str) -> Result<String, SftpError> {
        self.session
            .read_link(path)
            .await
            .map_err(|e| map_err(path, e))
    }

    /// 展开 ~ 等（依赖 expand-path@openssh.com，不支持的服务器返回 None）
    pub async fn expand_path(&self, path: &str) -> Result<Option<String>, SftpError> {
        self.session
            .expand_path(path)
            .await
            .map_err(|e| map_err(path, e))
    }
    /// 解析真实路径（REALPATH，SFTP v3 基础协议，所有服务器可用）
    pub async fn canonicalize(&self, path: &str) -> Result<String, SftpError> {
        self.session
            .canonicalize(path)
            .await
            .map_err(|e| map_err(path, e))
    }

    /// 低层传输原语：只读打开（TransferQueue 使用；一般调用方勿直接碰）
    #[doc(hidden)]
    pub async fn open_read(&self, path: &str) -> Result<File, SftpError> {
        self.session.open(path).await.map_err(|e| map_err(path, e))
    }

    /// 低层传输原语：写打开。offset>0 = 续传定位；offset=0 = 截断重建。
    #[doc(hidden)]
    pub async fn open_write_at(&self, path: &str, offset: u64) -> Result<File, SftpError> {
        use tokio::io::AsyncSeekExt;
        let mut f = if offset == 0 {
            self.session
                .create(path)
                .await
                .map_err(|e| map_err(path, e))?
        } else {
            let f = self
                .session
                .open_with_flags(path, OpenFlags::CREATE | OpenFlags::WRITE)
                .await
                .map_err(|e| map_err(path, e))?;
            f
        };
        if offset > 0 {
            f.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| map_err(path, e.into()))?;
        }
        Ok(f)
    }
}
