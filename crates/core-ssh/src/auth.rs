//! 认证方法。凭据材料以零化字符串保存，不落盘、不序列化（安全模型第 4 条）。

use zeroize::Zeroizing;

/// keyboard-interactive 的一次提问（服务器可连续多轮）
#[derive(Debug, Clone)]
pub struct KeyboardInteractivePrompt {
    pub name: String,
    pub instruction: String,
    pub prompt: String,
    pub echo: bool,
}

#[derive(Clone)]
pub enum AuthMethod {
    /// 仅测试与本地场景使用
    None,
    Password(Zeroizing<String>),
    /// 私钥内容（PEM/OpenSSH 格式）+ 可选 passphrase；.ppk 在 M1 转换层处理
    PublicKey {
        key_pem: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
    },
    /// 2FA：由上层（GUI 弹窗 / CLI 参数）逐轮应答
    KeyboardInteractive,
    /// Windows OpenSSH 命名管道 / Pageant
    Agent,
}

impl AuthMethod {
    pub fn name(&self) -> &'static str {
        match self {
            AuthMethod::None => "none",
            AuthMethod::Password(_) => "password",
            AuthMethod::PublicKey { .. } => "publickey",
            AuthMethod::KeyboardInteractive => "keyboard-interactive",
            AuthMethod::Agent => "agent",
        }
    }
}

// 凭据不明文进 Debug 日志
impl std::fmt::Debug for AuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthMethod")
            .field("method", &self.name())
            .finish_non_exhaustive()
    }
}
