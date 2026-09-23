//! 建连模块：会话解析后的连接类型（AuthSpec/TermOpenSpec/JumpHopSpec）+
//! 后台 Bulk 连接策略的唯一所有者（架构 review 卡 5）。
//!
//! 后台策略（隧道/SFTP/Exec/MCP 终端共享）：KI 一律拒绝（后台无交互应答通路）、
//! known_hosts 严格校验且未知/变更 fail-closed（AcceptAll 绝不可用——安全模型第 3 条；
//! 首连须在终端侧完成过指纹确认）、ConnClass::Bulk（不占交互连接）。
//! window 按用途分档：Throughput（16MB，隧道/SFTP，spike 验证值）与
//! Control（4MB，exec/MCP 终端，控制流量）——曾经的 16/4MB 漂移转正为显式 profile。

use std::sync::Arc;

use serde::Deserialize;
use zeroize::Zeroizing;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, HostKeyDecision, KeepaliveConfig,
    KnownHostsPolicy, SshConnection, SshError,
};

// ---------- 连接类型（自 terminal.rs 迁入；serde 形状不变，前端契约不受影响） ----------

/// 前端传入的认证材料（secret 只在内存停留，Zeroizing 落 core-ssh）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum AuthSpec {
    Password {
        password: String,
    },
    /// keyPem：OpenSSH/PKCS8/PKCS5/PuTTY .ppk 均可
    PublicKey {
        key_pem: String,
        passphrase: Option<String>,
    },
    KeyboardInteractive,
    Agent,
}

/// 一跳跳板（已解析的认证材料；由 sessions.rs 从档案+保险库解析注入）
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JumpHopSpec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthSpec,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TermOpenSpec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthSpec,
    /// ProxyJump 链（就近→最远）；空 = 直连
    #[serde(default)]
    pub jump_chain: Vec<JumpHopSpec>,
    /// 终端类型，默认 xterm-256color
    pub term: Option<String>,
    /// 启动命令；None = 登录 shell
    pub command: Option<String>,
    /// 终端编码（encoding_rs 标签）；默认 utf-8 = 直通不转码
    #[serde(default = "default_encoding")]
    pub encoding: String,
    /// 登录后切换用户（su）目标用户名；None/空 = 不切换（批次二十二）
    #[serde(default)]
    pub su_user: Option<String>,
    /// su 密码（内存经手即弃；档案路径由 resolve 从保险库读出）
    #[serde(default)]
    pub su_password: Option<String>,
    /// 登录宏：进 shell 后自动逐行执行的命令（多行文本）；None/空 = 不执行。
    /// 语义：无 su 即发；有 su 则密码应答后发；配 command 的会话不执行；重连重放
    #[serde(default)]
    pub login_macro: Option<String>,
}

fn default_encoding() -> String {
    "utf-8".into()
}

/// AuthSpec → core-ssh 认证材料（Zeroizing 包裹秘密）
pub(crate) fn auth_method_from(auth: &AuthSpec) -> AuthMethod {
    match auth {
        AuthSpec::Password { password } => AuthMethod::Password(Zeroizing::new(password.clone())),
        AuthSpec::PublicKey {
            key_pem,
            passphrase,
        } => AuthMethod::PublicKey {
            key_pem: Zeroizing::new(key_pem.clone()),
            passphrase: passphrase.clone().map(Zeroizing::new),
        },
        AuthSpec::KeyboardInteractive => AuthMethod::KeyboardInteractive,
        AuthSpec::Agent => AuthMethod::Agent,
    }
}

/// 跳板链 → core-ssh（KI 在跳板上同样弹窗——复用同一决策桥）
pub(crate) fn jump_chain_from(chain: &[JumpHopSpec]) -> Vec<core_ssh::JumpHop> {
    chain
        .iter()
        .map(|h| core_ssh::JumpHop {
            host: h.host.clone(),
            port: h.port,
            user: h.user.clone(),
            auth: auth_method_from(&h.auth),
        })
        .collect()
}

pub(crate) fn known_hosts_path() -> std::path::PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("myssh")
        .join("known_hosts")
}

// ---------- 后台 Bulk 连接策略 ----------

/// window 分档（Q1 裁决：漂移转正为显式 profile）
#[derive(Debug, Clone, Copy)]
pub(crate) enum ConnectProfile {
    /// 隧道/SFTP 大流量：16MB（spike 验证值；07 文档 4MB 基线系 50ms RTT 推算，
    /// 2026-08-23 环境回归期间实测非瓶颈（见 10-risks），保守取验证值）
    Throughput,
    /// exec/MCP 终端控制流量：4MB
    Control,
}

impl ConnectProfile {
    fn window_size(self) -> u32 {
        match self {
            Self::Throughput => 16 * 1024 * 1024,
            Self::Control => 4 * 1024 * 1024,
        }
    }
}

/// 后台连接无交互应答通路：KI 一律拒绝（主认证或任一跳板命中即拒）。
/// 错误固定为 UnsupportedAuth——重连分类（AuthFailed）语义比语境文案值钱，
/// 语境由调用方日志/错误包装承载。
pub(crate) fn reject_background_ki(spec: &TermOpenSpec) -> Result<(), SshError> {
    if matches!(spec.auth, AuthSpec::KeyboardInteractive)
        || spec
            .jump_chain
            .iter()
            .any(|h| matches!(h.auth, AuthSpec::KeyboardInteractive))
    {
        return Err(SshError::UnsupportedAuth(
            "keyboard-interactive 不适用于后台连接（请改用密钥/agent）",
        ));
    }
    Ok(())
}

/// 后台主机密钥策略：known_hosts 严格校验，未知/变更一律拒绝
///（无 UI 弹窗通路；用户须先经终端侧完成首连确认——与 FinalShell 一致）
pub(crate) fn background_host_key_check() -> HostKeyCheck {
    HostKeyCheck::KnownHosts(KnownHostsPolicy {
        path: known_hosts_path(),
        prompter: Arc::new(|_prompt| async { HostKeyDecision::Reject }),
    })
}

/// 后台 Bulk 连接的唯一入口（隧道/SFTP/Exec/MCP 终端共享）。
pub(crate) async fn background(
    spec: &TermOpenSpec,
    profile: ConnectProfile,
) -> Result<SshConnection, SshError> {
    reject_background_ki(spec)?;
    SshConnection::connect(ConnectOptions {
        host: spec.host.clone(),
        port: spec.port,
        user: spec.user.clone(),
        auth: auth_method_from(&spec.auth),
        jump_chain: jump_chain_from(&spec.jump_chain),
        class: ConnClass::Bulk,
        window_size: profile.window_size(),
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        host_key_check: background_host_key_check(),
        ki_prompter: None,
    })
    .await
}

/// 认证方式的结构化指纹：只含判别式——秘密永不入指纹；
/// 凭据轮换不拆隧道组（重连时经 resolve_session_spec 取新凭据）。
pub(crate) fn hash_auth<H: std::hash::Hasher>(h: &mut H, auth: &AuthSpec) {
    use std::hash::Hash;
    std::mem::discriminant(auth).hash(h);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use core_ssh::ReconnectClass;

    fn spec(auth: AuthSpec, chain: Vec<JumpHopSpec>) -> TermOpenSpec {
        TermOpenSpec {
            host: "h".into(),
            port: 22,
            user: "u".into(),
            auth,
            jump_chain: chain,
            term: None,
            command: None,
            encoding: "utf-8".into(),
            su_user: None,
            su_password: None,
            login_macro: None,
        }
    }

    /// KI 谓词：主认证或任一跳板命中即拒，且分类为 AuthFailed（不盲目重连）
    #[test]
    fn background_rejects_ki_on_main_and_hops() {
        let ki_main = spec(AuthSpec::KeyboardInteractive, vec![]);
        let e = reject_background_ki(&ki_main).unwrap_err();
        assert!(matches!(e, SshError::UnsupportedAuth(_)));
        assert_eq!(e.reconnect_class(), ReconnectClass::AuthFailed);

        let ki_hop = spec(
            AuthSpec::Agent,
            vec![
                JumpHopSpec {
                    host: "j1".into(),
                    port: 22,
                    user: "u".into(),
                    auth: AuthSpec::Password {
                        password: "x".into(),
                    },
                },
                JumpHopSpec {
                    host: "j2".into(),
                    port: 22,
                    user: "u".into(),
                    auth: AuthSpec::KeyboardInteractive,
                },
            ],
        );
        assert!(reject_background_ki(&ki_hop).is_err(), "跳板链含 KI 即拒");

        let ok = spec(AuthSpec::Agent, vec![]);
        assert!(reject_background_ki(&ok).is_ok());
    }

    /// profile → window：Throughput 16MB / Control 4MB
    #[test]
    fn profile_window_mapping() {
        assert_eq!(ConnectProfile::Throughput.window_size(), 16 * 1024 * 1024);
        assert_eq!(ConnectProfile::Control.window_size(), 4 * 1024 * 1024);
    }

    /// 指纹结构性：方式变更改指纹；密码值/密钥内容变更不改（凭据轮换不拆组）
    #[test]
    fn fingerprint_is_structural() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::Hasher;
        let fp = |auth: &AuthSpec| {
            let mut h = DefaultHasher::new();
            hash_auth(&mut h, auth);
            h.finish()
        };
        let p1 = AuthSpec::Password {
            password: "a".into(),
        };
        let p2 = AuthSpec::Password {
            password: "b".into(),
        };
        let k = AuthSpec::PublicKey {
            key_pem: "k".into(),
            passphrase: None,
        };
        assert_eq!(fp(&p1), fp(&p2), "密码值变更不拆组");
        assert_ne!(fp(&p1), fp(&k), "认证方式变更拆组");
    }
}
