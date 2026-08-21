//! 连接建立与生命周期。M0 范围：connect + none/password 认证 + 原始 exec 通道；
//! PTY/known_hosts 弹窗/ProxyJump 在 M1/M2 补齐。

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use russh::client;
use russh::keys::ssh_key;
use russh::{cipher, Preferred};

use crate::auth::AuthMethod;
use crate::error::SshError;

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

#[derive(Debug, Clone)]
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
    /// 生产实现由 app 层注入：known_hosts 校验 + 首连/变更弹窗确认。
    /// M0 冒烟测试用 AcceptAll（仅此场景）。
    pub host_key_check: HostKeyCheck,
}

#[derive(Debug, Clone)]
pub enum HostKeyCheck {
    /// 仅测试：接受一切主机密钥
    AcceptAll,
    // M1: KnownHosts { path, confirm_callback }
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
        other => return Err(SshError::UnsupportedAuth(other.name())),
    };
    if ok {
        Ok(())
    } else {
        Err(auth_failed(opts.auth.name()))
    }
}

struct ClientHandler {
    check: HostKeyCheck,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, _key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        match &self.check {
            HostKeyCheck::AcceptAll => Ok(true),
            // M1 起由 app 层注入 known_hosts 校验；此处默认拒绝，fail-closed
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
