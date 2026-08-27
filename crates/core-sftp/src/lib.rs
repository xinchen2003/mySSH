//! core-sftp：SFTP 客户端封装（russh-sftp）与队列化传输。
//!
//! 设计契约见 docs/design/02-core-api.md。
//! 传输走 Bulk 连接池（规格书第 6 条：不占用交互式连接）。
//!
//! 错误码段：E5xxx。

mod client;
mod error;
mod transfer;

pub use client::{DirEntry, EntryKind, SftpClient};
pub use error::SftpError;
pub use transfer::{
    rename_candidate, OnExists, ProgressFn, TransferId, TransferInfo, TransferQueue, TransferState,
};

/// 传输方向
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

/// 传输进度（断点续传状态由 core-store 持久化）
#[derive(Debug, Clone, Copy, Default)]
pub struct TransferProgress {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub rate: u64,
    pub resumable: bool,
}
