//! core-ssh：russh 封装层。
//!
//! 职责边界：连接生命周期（ConnectOptions/认证族/known_hosts 交互决策）、PTY 会话通道、
//! 连接分类（Interactive 独占 / Bulk 池化，规格书第 6 条）。
//! 不感知：UI、隧道中继、SFTP 细节（那些在 core-tunnel / core-sftp）。
//!
//! 错误码段：E1xxx 连接 / E2xxx 认证 / E3xxx 通道（统一错误码约定见 09-m0-plan）。

mod auth;
mod connection;
mod error;
mod hostkey;
mod pty;

pub use auth::{AuthMethod, KeyboardInteractivePrompt, KiChallenge, KiPrompter, SharedKiPrompter};
pub use connection::{ConnClass, ConnectOptions, KeepaliveConfig, SshConnection};
pub use error::SshError;
pub use hostkey::{
    HostKeyCheck, HostKeyDecision, HostKeyPrompt, HostKeyPrompter, HostKeyStatus, KnownHostsPolicy,
};
pub use pty::{PtyChannel, PtyReader, PtyWriter};
