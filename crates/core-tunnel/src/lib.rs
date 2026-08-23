//! core-tunnel：隧道管理器（本地 -L / 动态 SOCKS5 -D / 远程 -R）。
//!
//! 独立线程 + 独立 runtime；数据路径零 IPC 有界背压；监督器重连。
//! 设计：docs/design/05-connection-pool.md、07-tunnel-config.md。
//!
//! M2 范围注记：每隧道单条 Bulk 连接（loopback 实测 1277/438MB/s，超 400MB/s 预算）；
//! 多连接池化（05 文档 pool_scale_threshold）待实测饱和信号出现后落地。
//!
//! 错误码段：E4xxx。

mod error;
mod manager;

pub use error::TunnelError;
pub use manager::{
    ConnectFn, DisconnectPolicy, StatsAtomic, TunnelInfo, TunnelKind, TunnelManager, TunnelSpec,
    TunnelStats, TunnelStatus,
};
