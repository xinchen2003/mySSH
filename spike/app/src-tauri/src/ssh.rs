//! russh 客户端：交互式连接（终端 + echo 探针通道）。

use std::borrow::Cow;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use russh::client::{self, AuthResult};
use russh::keys::ssh_key;
use russh::{cipher, Preferred};

/// 接收窗口：按规格书第 12 条，高带宽时延积链路 8~16MB
pub const WINDOW_SIZE: u32 = 16 * 1024 * 1024;
pub const MAX_PACKET: u32 = 32768;

pub struct ClientHandler;

impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, _key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        // spike：loopback 一次性密钥直接接受；生产实现走 known_hosts + 弹窗确认
        Ok(true)
    }
}

pub type ClientHandle = Arc<client::Handle<ClientHandler>>;

fn client_config() -> client::Config {
    let mut preferred = Preferred::default();
    preferred.cipher = Cow::Borrowed(&[
        cipher::AES_256_GCM,
        cipher::CHACHA20_POLY1305,
        cipher::AES_256_CTR,
    ]);
    client::Config {
        window_size: WINDOW_SIZE,
        maximum_packet_size: MAX_PACKET,
        channel_buffer_size: 1024,
        nodelay: true,
        inactivity_timeout: None,
        preferred,
        ..Default::default()
    }
}

pub async fn connect() -> Result<ClientHandle> {
    let mut handle =
        client::connect(Arc::new(client_config()), ("127.0.0.1", 2222u16), ClientHandler).await?;
    let result: AuthResult = handle.authenticate_none("spike").await?;
    if !result.success() {
        return Err(anyhow!("server rejected none auth"));
    }
    Ok(Arc::new(handle))
}
