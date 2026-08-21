//! core-tunnel：隧道管理（监听 / 中继 / 连接池 / 重连 supervisor）。
//!
//! 设计契约见 docs/design/04-dataflow.md（缓冲账）与 05-connection-pool.md（池策略）。
//! M0 为空壳：类型与错误码先行，实现随 M2 落地。
//!
//! 错误码段：E4xxx。

mod error;
mod manager;

pub use error::TunnelError;
pub use manager::{
    DisconnectPolicy, TunnelKind, TunnelManager, TunnelSpec, TunnelStats, TunnelStatus,
};
