//! 语义化退出码（规格书 M6：进入兼容性契约，变更需版本化）

#[derive(Debug, Clone, Copy)]
#[repr(i32)]
pub enum ExitCode {
    /// 成功
    Ok = 0,
    /// 命令执行失败（远端非零退出）
    RemoteFailure = 1,
    /// 参数错误（clap 自行处理大部分，预留）
    #[allow(dead_code)]
    Usage = 2,
    /// 连接失败
    #[allow(dead_code)]
    ConnectFailed = 3,
    /// 认证失败
    #[allow(dead_code)]
    AuthFailed = 4,
    /// 权限拒绝（core-policy）
    #[allow(dead_code)]
    PermissionDenied = 5,
    /// 超时
    #[allow(dead_code)]
    Timeout = 6,
    /// 用户取消
    #[allow(dead_code)]
    Cancelled = 7,
}
