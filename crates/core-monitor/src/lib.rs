//! 服务器监控：/proc 指标采集（M4）。
//!
//! 不依赖目标机安装 agent：每轮采集在一个独立 exec channel 上执行一段
//! POSIX sh 组合脚本（cat /proc/stat|meminfo|loadavg|diskstats|net/dev + ps），
//! 解析器为纯函数。速率类指标（CPU 忙率、磁盘/网络 IO）基于相邻两轮差分，
//! 首轮相应字段为 None。采集失败返回错误由上层静默降级，绝不影响终端。

mod collector;
pub mod error;
mod parser;
mod types;

pub use collector::MetricsCollector;
pub use error::MonitorError;
pub use types::{DiskRate, MetricsSnapshot, NetRate, ProcInfo};
