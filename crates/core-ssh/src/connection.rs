//! 连接建立与生命周期。
//!
//! M1 范围：认证族（password / publickey 含 .ppk / keyboard-interactive / agent）、
//! known_hosts 校验（首连与变更交互决策）、PTY 通道打开。
//! ProxyJump 在 M2 补齐。

use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::keys::ssh_key;
use russh::keys::{decode_secret_key, PrivateKeyWithHashAlg};
use russh::{cipher, Preferred};

use crate::auth::{AuthMethod, KeyboardInteractivePrompt, KiChallenge, SharedKiPrompter};
use crate::error::SshError;
use crate::hostkey::{self, HostKeyCheck, HostKeyDecision, HostKeyPrompt, HostKeyStatus};
use crate::pty::PtyChannel;

/// 连接用途分类——决定 runtime 归属与是否允许共享（规格书第 6 条）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnClass {
    /// 交互式终端：独占连接，绝不承载隧道/批量/AI 流量
    Interactive,
    /// 隧道/SFTP/AI：走 Bulk 连接池
    Bulk,
}

#[derive(Debug, Clone)]
pub struct KeepaliveConfig {
    pub interval: Duration,
    pub max: usize,
}

impl Default for KeepaliveConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(15),
            max: 3,
        }
    }
}

#[derive(Clone)]
pub struct ConnectOptions {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: AuthMethod,
    pub class: ConnClass,
    /// 通道接收窗口；隧道默认 4MB、终端默认 4MB（内存账见 05/07 设计文档）
    pub window_size: u32,
    pub max_packet_size: u32,
    pub keepalive: KeepaliveConfig,
    /// known_hosts 校验 + 首连/变更决策回调（AcceptAll 仅限测试）
    pub host_key_check: HostKeyCheck,
    /// AuthMethod::KeyboardInteractive 时必需：逐轮应答回调
    pub ki_prompter: Option<SharedKiPrompter>,
}

impl fmt::Debug for ConnectOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectOptions")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("auth", &self.auth)
            .field("class", &self.class)
            .field("window_size", &self.window_size)
            .field("max_packet_size", &self.max_packet_size)
            .field("keepalive", &self.keepalive)
            .field("host_key_check", &self.host_key_check)
            .field("ki_prompter", &self.ki_prompter.as_ref().map(|_| "<set>"))
            .finish()
    }
}

pub struct SshConnection {
    handle: Arc<client::Handle<ClientHandler>>,
    class: ConnClass,
}

impl SshConnection {
    pub async fn connect(opts: ConnectOptions) -> Result<Self, SshError> {
        let config = client_config(&opts);
        let handler = ClientHandler {
            check: opts.host_key_check.clone(),
            host: opts.host.clone(),
            port: opts.port,
        };
        let target = (opts.host.as_str(), opts.port);
        let mut handle = client::connect(Arc::new(config), target, handler)
            .await
            .map_err(|e| SshError::Connect {
                target: format!("{}:{}", opts.host, opts.port),
                source: std::io::Error::other(e.to_string()),
            })?;

        authenticate(&mut handle, &opts).await?;
        Ok(Self {
            handle: Arc::new(handle),
            class: opts.class,
        })
    }

    pub fn class(&self) -> ConnClass {
        self.class
    }

    /// 打开交互式 PTY 通道并启动 shell 或指定命令。
    /// `term` 通常为 "xterm-256color"；像素维度传 0。
    pub async fn open_pty(
        &self,
        term: &str,
        cols: u32,
        rows: u32,
        command: Option<&str>,
    ) -> Result<PtyChannel, SshError> {
        let ch = self
            .handle
            .channel_open_session()
            .await
            .map_err(|e| SshError::ChannelOpen {
                kind: "session",
                reason: e.to_string(),
            })?;
        ch.request_pty(true, term, cols, rows, 0, 0, &[])
            .await
            .map_err(|e| SshError::ChannelOpen {
                kind: "pty",
                reason: e.to_string(),
            })?;
        match command {
            Some(cmd) => ch
                .exec(true, cmd)
                .await
                .map_err(|e| SshError::ChannelOpen {
                    kind: "exec",
                    reason: e.to_string(),
                })?,
            None => ch
                .request_shell(true)
                .await
                .map_err(|e| SshError::ChannelOpen {
                    kind: "shell",
                    reason: e.to_string(),
                })?,
        }
        let (read, write) = ch.split();
        Ok(PtyChannel::new(read, write))
    }

    /// M0 测试辅助：打开会话通道执行命令，收集 stdout 到 EOF。
    pub async fn exec_collect(&self, command: &str) -> Result<Vec<u8>, SshError> {
        let mut ch =
            self.handle
                .channel_open_session()
                .await
                .map_err(|e| SshError::ChannelOpen {
                    kind: "session",
                    reason: e.to_string(),
                })?;
        ch.exec(true, command)
            .await
            .map_err(|e| SshError::ChannelOpen {
                kind: "exec",
                reason: e.to_string(),
            })?;
        let mut out = Vec::new();
        while let Some(msg) = ch.wait().await {
            match msg {
                russh::ChannelMsg::Data { data } => out.extend_from_slice(&data),
                russh::ChannelMsg::Eof | russh::ChannelMsg::Close => break,
                _ => {}
            }
        }
        Ok(out)
    }
}

async fn authenticate(
    handle: &mut client::Handle<ClientHandler>,
    opts: &ConnectOptions,
) -> Result<(), SshError> {
    let auth_failed = |method: &str| SshError::AuthFailed {
        user: opts.user.clone(),
        host: format!("{}:{}", opts.host, opts.port),
        method: method.to_string(),
    };
    let ok = match &opts.auth {
        AuthMethod::None => handle
            .authenticate_none(&opts.user)
            .await
            .map_err(|_| auth_failed("none"))?
            .success(),
        AuthMethod::Password(pw) => handle
            .authenticate_password(&opts.user, pw.as_str())
            .await
            .map_err(|_| auth_failed("password"))?
            .success(),
        AuthMethod::PublicKey {
            key_pem,
            passphrase,
        } => {
            // OpenSSH / PKCS8 / PKCS5 / PuTTY .ppk 统一由 decode_secret_key 解析
            let key = decode_secret_key(key_pem.as_str(), passphrase.as_ref().map(|p| p.as_str()))
                .map_err(|e| SshError::CredentialUnavailable(format!("私钥解析失败: {e}")))?;
            let hash_alg = if key.algorithm().is_rsa() {
                handle
                    .best_supported_rsa_hash()
                    .await
                    .ok()
                    .flatten()
                    .flatten()
            } else {
                None
            };
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
            handle
                .authenticate_publickey(&opts.user, key)
                .await
                .map_err(|e| SshError::CredentialUnavailable(format!("公钥认证失败: {e}")))?
                .success()
        }
        AuthMethod::KeyboardInteractive => {
            auth_keyboard_interactive(handle, opts).await?;
            true
        }
        AuthMethod::Agent => auth_agent(handle, opts).await?,
    };
    if ok {
        Ok(())
    } else {
        Err(auth_failed(opts.auth.name()))
    }
}

async fn auth_keyboard_interactive(
    handle: &mut client::Handle<ClientHandler>,
    opts: &ConnectOptions,
) -> Result<(), SshError> {
    let prompter = opts.ki_prompter.as_ref().ok_or_else(|| {
        SshError::CredentialUnavailable("keyboard-interactive 缺少应答回调".into())
    })?;
    let auth_failed = || SshError::AuthFailed {
        user: opts.user.clone(),
        host: format!("{}:{}", opts.host, opts.port),
        method: "keyboard-interactive".to_string(),
    };
    let mut response = handle
        .authenticate_keyboard_interactive_start(&opts.user, None)
        .await
        .map_err(|_| auth_failed())?;
    loop {
        response = match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(()),
            KeyboardInteractiveAuthResponse::Failure { .. } => return Err(auth_failed()),
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let challenge = KiChallenge {
                    name,
                    instruction: instructions,
                    prompts: prompts
                        .into_iter()
                        .map(|p| KeyboardInteractivePrompt {
                            prompt: p.prompt,
                            echo: p.echo,
                        })
                        .collect(),
                };
                let answers = prompter.respond(challenge).await.ok_or_else(|| {
                    SshError::CredentialUnavailable("keyboard-interactive 应答被取消".into())
                })?;
                handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(|_| auth_failed())?
            }
        };
    }
}

/// agent 认证：Windows 依次尝试 OpenSSH 命名管道与 Pageant；Unix 走 $SSH_AUTH_SOCK。
/// 逐个尝试 agent 中的身份，任一成功即返回 true；agent 不可达返回 false（不视为致命）。
#[cfg(windows)]
async fn auth_agent(
    handle: &mut client::Handle<ClientHandler>,
    opts: &ConnectOptions,
) -> Result<bool, SshError> {
    use russh::keys::agent::client::AgentClient;

    // OpenSSH agent（命名管道）→ Pageant
    let mut agent = match AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await {
        Ok(a) => a.dynamic(),
        Err(_) => match AgentClient::connect_pageant().await {
            Ok(a) => a.dynamic(),
            Err(e) => {
                return Err(SshError::CredentialUnavailable(format!(
                    "未找到可用的 ssh-agent（命名管道/Pageant）: {e}"
                )));
            }
        },
    };
    agent_auth_loop(handle, opts, &mut agent).await
}

#[cfg(unix)]
async fn auth_agent(
    handle: &mut client::Handle<ClientHandler>,
    opts: &ConnectOptions,
) -> Result<bool, SshError> {
    use russh::keys::agent::client::AgentClient;

    let sock = std::env::var_os("SSH_AUTH_SOCK")
        .ok_or_else(|| SshError::CredentialUnavailable("SSH_AUTH_SOCK 未设置".into()))?;
    let mut agent = AgentClient::connect_uds(sock)
        .await
        .map_err(|e| SshError::CredentialUnavailable(format!("ssh-agent 连接失败: {e}")))?
        .dynamic();
    agent_auth_loop(handle, opts, &mut agent).await
}

async fn agent_auth_loop<S>(
    handle: &mut client::Handle<ClientHandler>,
    opts: &ConnectOptions,
    agent: &mut russh::keys::agent::client::AgentClient<S>,
) -> Result<bool, SshError>
where
    S: russh::keys::agent::client::AgentStream + Send + Unpin,
{
    use russh::keys::agent::AgentIdentity;

    let identities = agent
        .request_identities()
        .await
        .map_err(|e| SshError::CredentialUnavailable(format!("agent 身份列表获取失败: {e}")))?;
    for identity in identities {
        // M1 只尝试纯公钥身份；证书身份在 M5 视需求补
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue;
        };
        let hash_alg = if key.algorithm().is_rsa() {
            handle
                .best_supported_rsa_hash()
                .await
                .ok()
                .flatten()
                .flatten()
        } else {
            None
        };
        match handle
            .authenticate_publickey_with(&opts.user, key, hash_alg, agent)
            .await
        {
            Ok(result) if result.success() => return Ok(true),
            // 该身份被拒或签名失败：尝试下一个
            _ => continue,
        }
    }
    Ok(false)
}

struct ClientHandler {
    check: HostKeyCheck,
    host: String,
    port: u16,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        match &self.check {
            HostKeyCheck::AcceptAll => Ok(true),
            HostKeyCheck::KnownHosts(policy) => {
                // 评估 IO 失败：fail-closed 拒绝（视为环境异常，不重试弹窗）
                let status = match hostkey::evaluate(&policy.path, &self.host, self.port, key) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "known_hosts 评估失败，拒绝连接");
                        return Ok(false);
                    }
                };
                let prompt = match status {
                    HostKeyStatus::Trusted => return Ok(true),
                    HostKeyStatus::Unknown => HostKeyPrompt::Unknown {
                        host: self.host.clone(),
                        port: self.port,
                        key_type: key.algorithm().to_string(),
                        fingerprint: hostkey::fingerprint(key),
                    },
                    HostKeyStatus::Changed { old_fingerprint } => HostKeyPrompt::Changed {
                        host: self.host.clone(),
                        port: self.port,
                        key_type: key.algorithm().to_string(),
                        old_fingerprint,
                        new_fingerprint: hostkey::fingerprint(key),
                    },
                };
                match policy.prompter.prompt(prompt).await {
                    HostKeyDecision::Learn => {
                        // 变更情形先清旧记录再写入，避免旧条目持续触发 KeyChanged。
                        // 落盘失败不否决用户已批准的本次连接（下次连接会重新弹窗）。
                        if let Err(e) =
                            hostkey::remove_host_keys(&policy.path, &self.host, self.port).and_then(
                                |_| hostkey::learn(&policy.path, &self.host, self.port, key),
                            )
                        {
                            tracing::error!(error = %e, "known_hosts 写入失败");
                        }
                        Ok(true)
                    }
                    HostKeyDecision::Reject => Ok(false),
                }
            }
        }
    }
}

fn client_config(opts: &ConnectOptions) -> client::Config {
    // 有 AES-NI 优先 GCM（规格书第 13 条）；ring 运行时分派 SIMD，无需编译期探测
    let preferred = Preferred {
        cipher: Cow::Borrowed(&[
            cipher::AES_256_GCM,
            cipher::CHACHA20_POLY1305,
            cipher::AES_256_CTR,
        ]),
        ..Default::default()
    };
    client::Config {
        window_size: opts.window_size,
        maximum_packet_size: opts.max_packet_size,
        // 消息数上界 = 窗口/包长：用户态排队内存与窗口值对齐（05 内存账）
        channel_buffer_size: (opts.window_size / opts.max_packet_size).max(1) as usize,
        nodelay: true,
        inactivity_timeout: None,
        keepalive_interval: Some(opts.keepalive.interval),
        keepalive_max: opts.keepalive.max,
        preferred,
        ..Default::default()
    }
}
