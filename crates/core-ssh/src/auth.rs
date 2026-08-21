//! 认证方法。凭据材料以零化字符串保存，不落盘、不序列化（安全模型第 4 条）。

use std::future::Future;
use std::sync::Arc;

use zeroize::Zeroizing;

/// keyboard-interactive 的单个提问（echo=false 时输入不回显）
#[derive(Debug, Clone)]
pub struct KeyboardInteractivePrompt {
    pub prompt: String,
    pub echo: bool,
}

/// 一轮 keyboard-interactive 质询：服务器可连续多轮，每轮若干提问
#[derive(Debug, Clone)]
pub struct KiChallenge {
    pub name: String,
    pub instruction: String,
    pub prompts: Vec<KeyboardInteractivePrompt>,
}

/// KI 应答回调（GUI 弹窗 / CLI 询问）。返回 None 表示用户取消，认证中止。
/// 答案数量必须与提问数量一致。
pub trait KiPrompter: Send + Sync {
    fn respond(
        &self,
        challenge: KiChallenge,
    ) -> std::pin::Pin<Box<dyn Future<Output = Option<Vec<String>>> + Send + '_>>;
}

// 闭包即 Prompter
impl<F, Fut> KiPrompter for F
where
    F: Fn(KiChallenge) -> Fut + Send + Sync,
    Fut: Future<Output = Option<Vec<String>>> + Send + 'static,
{
    fn respond(
        &self,
        challenge: KiChallenge,
    ) -> std::pin::Pin<Box<dyn Future<Output = Option<Vec<String>>> + Send + '_>> {
        Box::pin(self(challenge))
    }
}

/// KI Prompter 的共享句柄类型（ConnectOptions 字段）
pub type SharedKiPrompter = Arc<dyn KiPrompter>;

#[derive(Clone)]
pub enum AuthMethod {
    /// 仅测试与本地场景使用
    None,
    Password(Zeroizing<String>),
    /// 私钥内容（OpenSSH/PKCS8/PKCS5/PuTTY .ppk 均可，由 russh decode_secret_key 统一解析）
    /// + 可选 passphrase
    PublicKey {
        key_pem: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
    },
    /// 2FA：由 KiPrompter 逐轮应答
    KeyboardInteractive,
    /// Windows OpenSSH 命名管道 / Pageant；Unix 走 $SSH_AUTH_SOCK
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
