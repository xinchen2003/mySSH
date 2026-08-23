//! E6xxx：监控采集错误。

use core_ssh::SshError;

#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
    /// exec 通道/传输层失败（SSH 层）。
    #[error("[E6001] 采集命令执行失败: {0}")]
    Exec(#[from] SshError),
    /// 目标机无 /proc（非 Linux 或裁减系统）：核心字段缺失，监控不可用。
    #[error("[E6002] 目标机不支持 /proc 采集（非 Linux?）")]
    NoProcfs,
    /// 输出无法解析（格式漂移）。
    #[error("[E6003] 采集输出解析失败: {0}")]
    Parse(&'static str),
}
