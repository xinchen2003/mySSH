//! core-policy：AI 访问的权限引擎（规格书安全模型，优先级最高、不可妥协）。
//!
//! 核心前提：远程返回的输出是不可信输入；所有防线在本层，不依赖模型自觉。
//!
//! 判定流水线（对应 docs/design/08-ai-interface.md 第 3 节矩阵）：
//!   1. 解析失败（引号/括号不配对等）→ RequireConfirmation（fail-closed）
//!   2. 任一子命令命中全局黑名单 → RequireConfirmation（任何档位，不可静默）
//!   3. 间接执行（bash -c / eval / xargs 等）→ RequireConfirmation
//!   4. 全部子命令在只读白名单 → 按档位 Allow
//!   5. 存在「无法证明只读」的子命令 → readonly: Deny / standard/full: RequireConfirmation
//!   6. production 会话 + 非全白名单 → 强制 RequireConfirmation（覆盖一切，不可配置绕过）
//!
//! M0 注意：分类器是保守近似——不能证明只读的命令一律按潜在写操作处理；
//! 矩阵中 full 档的「Allow*」依赖 M6 的「用户显式关闭确认」选项，本版不提供。
//! 白名单不是完备防御，仅纵深防御的一层（规格书明示）。
//!
//! 错误码段：E6xxx。

mod classifier;
mod error;
mod rate;

pub use classifier::{AccessLevel, PolicyEngine, Verdict};
pub use error::PolicyError;
pub use rate::{CallerId, RateLimiter};
