//! known_hosts 校验与首连/变更决策。
//!
//! 安全模型（规格书第 3 条）：默认 known_hosts 严格校验；首连弹窗确认指纹；
//! 密钥变更弹窗硬警告，用户确认后先移除旧条目再记录新条目。
//! `AcceptAll` 仅限测试与内存服务端冒烟。

use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh::keys::known_hosts::{
    check_known_hosts_path, known_host_keys_path, learn_known_hosts_path,
};
use russh::keys::ssh_key::{HashAlg, PublicKey};

use crate::error::SshError;

/// 主机密钥校验策略
#[derive(Clone)]
pub enum HostKeyCheck {
    /// 仅测试：接受一切主机密钥
    AcceptAll,
    /// 生产路径：known_hosts 文件校验 + 交互决策
    KnownHosts(KnownHostsPolicy),
}

impl fmt::Debug for HostKeyCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HostKeyCheck::AcceptAll => f.write_str("AcceptAll"),
            HostKeyCheck::KnownHosts(p) => f
                .debug_struct("KnownHosts")
                .field("path", &p.path)
                .finish_non_exhaustive(),
        }
    }
}

#[derive(Clone)]
pub struct KnownHostsPolicy {
    /// known_hosts 文件路径（默认 %APPDATA%/myssh/known_hosts，由 app 层注入）
    pub path: PathBuf,
    /// 首连/变更时的交互决策回调（GUI 弹窗或 CLI 询问；不得阻塞，内部走 IPC）
    pub prompter: Arc<dyn HostKeyPrompter>,
}

/// 提请用户决策的两种情形（指纹为 OpenSSH 风格 SHA256 base64）
#[derive(Debug, Clone)]
pub enum HostKeyPrompt {
    /// 首次连接：known_hosts 无此主机记录
    Unknown {
        host: String,
        port: u16,
        key_type: String,
        fingerprint: String,
    },
    /// 密钥变更：known_hosts 中记录与新密钥同类型但不一致——可能是中间人攻击
    Changed {
        host: String,
        port: u16,
        key_type: String,
        old_fingerprint: String,
        new_fingerprint: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    /// 信任并写入 known_hosts（Changed 情形会先移除旧记录）
    Learn,
    Reject,
}

/// 决策回调。返回 future 以便 app 层异步等待 GUI 弹窗结果。
/// 禁止在实现里直接阻塞 russh 任务。
pub trait HostKeyPrompter: Send + Sync {
    fn prompt(
        &self,
        prompt: HostKeyPrompt,
    ) -> std::pin::Pin<Box<dyn Future<Output = HostKeyDecision> + Send + '_>>;
}

// 闭包即 Prompter：测试与 app 层都以此注入
impl<F, Fut> HostKeyPrompter for F
where
    F: Fn(HostKeyPrompt) -> Fut + Send + Sync,
    Fut: Future<Output = HostKeyDecision> + Send + 'static,
{
    fn prompt(
        &self,
        prompt: HostKeyPrompt,
    ) -> std::pin::Pin<Box<dyn Future<Output = HostKeyDecision> + Send + '_>> {
        Box::pin(self(prompt))
    }
}

/// 校验结果（check_known_hosts_path 语义展开）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyStatus {
    /// 记录存在且密钥一致
    Trusted,
    /// 无此主机记录
    Unknown,
    /// 存在同算法记录但密钥不一致
    Changed { old_fingerprint: String },
}

/// OpenSSH 风格 SHA256 指纹（弹窗展示用）
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// 对 known_hosts 做静态评估（不触发交互）
pub fn evaluate(
    path: &Path,
    host: &str,
    port: u16,
    key: &PublicKey,
) -> Result<HostKeyStatus, SshError> {
    match check_known_hosts_path(host, port, key, path) {
        Ok(true) => Ok(HostKeyStatus::Trusted),
        Ok(false) => Ok(HostKeyStatus::Unknown),
        Err(russh::keys::Error::KeyChanged { .. }) => {
            let old_fingerprint = known_host_keys_path(host, port, path)
                .ok()
                .and_then(|keys| keys.into_iter().next())
                .map(|(_, k)| fingerprint(&k))
                .unwrap_or_else(|| "<无法读取旧指纹>".to_string());
            Ok(HostKeyStatus::Changed { old_fingerprint })
        }
        Err(e) => Err(SshError::HostKeyRejected {
            host: format!("{host}:{port}"),
            detail: e.to_string(),
        }),
    }
}

/// 移除 known_hosts 中某主机的全部记录（变更后重写前的清理）。
/// 仅支持明文主机条目；哈希条目（|1| 开头）按原样保留并计为未匹配。
pub fn remove_host_keys(path: &Path, host: &str, port: u16) -> Result<usize, SshError> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => {
            return Err(SshError::HostKeyStore {
                host: format!("{host}:{port}"),
                detail: e.to_string(),
            })
        }
    };
    let want = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let mut removed = 0;
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| {
            let first = line.split_whitespace().next().unwrap_or("");
            // 一行可逗号分隔多个主机模式
            let matched = !first.starts_with('|') && first.split(',').any(|h| h == want);
            if matched {
                removed += 1;
            }
            !matched
        })
        .collect();
    if removed > 0 {
        let mut out = kept.join("\n");
        if !out.is_empty() {
            out.push('\n');
        }
        std::fs::write(path, out).map_err(|e| SshError::HostKeyStore {
            host: format!("{host}:{port}"),
            detail: e.to_string(),
        })?;
    }
    Ok(removed)
}

/// 记录主机密钥（Changed 情形调用方须先 remove_host_keys）
pub fn learn(path: &Path, host: &str, port: u16, key: &PublicKey) -> Result<(), SshError> {
    learn_known_hosts_path(host, port, key, path).map_err(|e| SshError::HostKeyStore {
        host: format!("{host}:{port}"),
        detail: e.to_string(),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use russh::keys::{Algorithm, PrivateKey};

    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("myssh-hostkey-test-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("known_hosts")
    }

    fn gen_key() -> PublicKey {
        PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
            .unwrap()
            .public_key()
            .clone()
    }

    #[test]
    fn learn_then_trusted() {
        let path = temp_path("learn");
        let key = gen_key();
        assert_eq!(
            evaluate(&path, "example.com", 22, &key).unwrap(),
            HostKeyStatus::Unknown
        );
        learn(&path, "example.com", 22, &key).unwrap();
        assert_eq!(
            evaluate(&path, "example.com", 22, &key).unwrap(),
            HostKeyStatus::Trusted
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn changed_detection_and_removal() {
        let path = temp_path("changed");
        let key_a = gen_key();
        let key_b = gen_key();
        learn(&path, "example.com", 22, &key_a).unwrap();

        match evaluate(&path, "example.com", 22, &key_b).unwrap() {
            HostKeyStatus::Changed { old_fingerprint } => {
                assert_eq!(old_fingerprint, fingerprint(&key_a));
            }
            other => panic!("expected Changed, got {other:?}"),
        }

        // 移除旧记录 → 新密钥变为 Unknown；再 learn → Trusted
        let removed = remove_host_keys(&path, "example.com", 22).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(
            evaluate(&path, "example.com", 22, &key_b).unwrap(),
            HostKeyStatus::Unknown
        );
        learn(&path, "example.com", 22, &key_b).unwrap();
        assert_eq!(
            evaluate(&path, "example.com", 22, &key_b).unwrap(),
            HostKeyStatus::Trusted
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn non_default_port_uses_bracket_format() {
        let path = temp_path("port");
        let key = gen_key();
        learn(&path, "example.com", 2222, &key).unwrap();
        assert_eq!(
            evaluate(&path, "example.com", 2222, &key).unwrap(),
            HostKeyStatus::Trusted
        );
        // 不同端口互不影响
        assert_eq!(
            evaluate(&path, "example.com", 22, &key).unwrap(),
            HostKeyStatus::Unknown
        );
        assert_eq!(remove_host_keys(&path, "example.com", 2222).unwrap(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn remove_on_missing_file_is_zero() {
        let path = temp_path("missing").join("nonexistent").join("known_hosts");
        assert_eq!(remove_host_keys(&path, "h", 22).unwrap(), 0);
    }
}
