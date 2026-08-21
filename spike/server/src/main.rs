//! Spike SSH 测试服务端（仅 loopback，无认证，一次性密钥）。
//!
//! 协议（exec 命令）：
//!   stream <n>  —— 向 channel 推送 n 字节模式化文本后 EOF+close（模拟 `cat 大文件`）
//!   echo        —— 回显收到的所有数据（用于按键回显延迟测量）
//! direct-tcpip  —— 桥接到目标 host:port（隧道压测的对端）
//! 内置对端：127.0.0.1:9999 sink（丢弃并计数）、127.0.0.1:9998 source（洪水发送）

use std::borrow::Cow;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Server, Session};
use russh::{cipher, Channel, ChannelId, ChannelOpenFailure, MethodKind, MethodSet, Preferred};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing::{info, warn};

const SSH_ADDR: &str = "127.0.0.1:2222";
const SINK_ADDR: &str = "127.0.0.1:9999";
const SOURCE_ADDR: &str = "127.0.0.1:9998";

/// 16MB 窗口：按规格书第 12 条针对高带宽时延积链路调优
const WINDOW_SIZE: u32 = 16 * 1024 * 1024;
const MAX_PACKET: u32 = 32768;
/// 流式发送的分片大小，对齐 channel max_packet_size
const CHUNK: usize = 32 * 1024;

static SINK_BYTES: AtomicU64 = AtomicU64::new(0);
static SOURCE_BYTES: AtomicU64 = AtomicU64::new(0);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,russh=warn".into()),
        )
        .init();

    tokio::spawn(run_sink());
    tokio::spawn(run_source());
    tokio::spawn(report_stats());

    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)?;
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);

    let mut preferred = Preferred::default();
    preferred.cipher = Cow::Borrowed(&[
        cipher::AES_256_GCM,
        cipher::CHACHA20_POLY1305,
        cipher::AES_256_CTR,
    ]);

    let config = Config {
        methods,
        keys: vec![key],
        window_size: WINDOW_SIZE,
        maximum_packet_size: MAX_PACKET,
        channel_buffer_size: 1024,
        event_buffer_size: 4096,
        inactivity_timeout: None,
        nodelay: true,
        preferred,
        ..Default::default()
    };

    let listener = TcpListener::bind(SSH_ADDR).await?;
    info!(addr = SSH_ADDR, "spike SSH server listening");
    info!(sink = SINK_ADDR, source = SOURCE_ADDR, "test endpoints up");

    let mut server = SpikeServer;
    server.run_on_socket(Arc::new(config), &listener).await?;
    Ok(())
}

struct SpikeServer;

impl Server for SpikeServer {
    type Handler = ConnHandler;

    fn new_client(&mut self, _peer: Option<SocketAddr>) -> ConnHandler {
        ConnHandler {
            echo_channels: HashSet::new(),
        }
    }
}

struct ConnHandler {
    echo_channels: HashSet<ChannelId>,
}

impl Handler for ConnHandler {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data);
        let mut parts = cmd.split_whitespace();
        match parts.next() {
            Some("stream") => {
                let total: u64 = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(100 * 1024 * 1024);
                session.channel_success(channel)?;
                let handle = session.handle();
                tokio::spawn(async move {
                    if let Err(e) = stream_bytes(&handle, channel, total).await {
                        warn!(?e, "stream task failed");
                    }
                });
            }
            Some("echo") => {
                self.echo_channels.insert(channel);
                session.channel_success(channel)?;
            }
            _ => {
                session.channel_failure(channel)?;
            }
        }
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.echo_channels.contains(&channel) {
            // 回显路径：测量用，保持最直白的转发
            session
                .handle()
                .data(channel, data.to_vec())
                .await
                .map_err(|_| anyhow::anyhow!("echo channel closed"))?;
        }
        Ok(())
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.echo_channels.remove(&channel) {
            let handle = session.handle();
            handle.eof(channel).await.ok();
            handle.close(channel).await.ok();
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator_addr: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        match TcpStream::connect((host, port as u16)).await {
            Ok(tcp) => {
                let _ = tcp.set_nodelay(true);
                reply.accept().await;
                tokio::spawn(async move {
                    let mut stream = channel.into_stream();
                    let mut tcp = tcp;
                    match tokio::io::copy_bidirectional(&mut stream, &mut tcp).await {
                        Ok((a, b)) => info!(up = a, down = b, "direct-tcpip closed"),
                        Err(e) => warn!(?e, "direct-tcpip bridge error"),
                    }
                });
            }
            Err(e) => {
                warn!(?e, host, port, "direct-tcpip target unreachable");
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
            }
        }
        Ok(())
    }
}

/// 向 channel 推送 total 字节模式化文本。
/// 内容用真实文本行（带行号），让 xterm 解析负载贴近真实 cat 场景。
async fn stream_bytes(
    handle: &russh::server::Handle,
    channel: ChannelId,
    total: u64,
) -> Result<()> {
    let pattern = build_pattern();
    let mut sent: u64 = 0;
    let started = std::time::Instant::now();

    while sent < total {
        let remaining = (total - sent) as usize;
        let n = remaining.min(CHUNK);
        // 从模式块循环切片（零拷贝：Bytes::slice 共享底层缓冲）
        let off = (sent as usize) % pattern.len();
        let avail = pattern.len() - off;
        let end = off + n.min(avail);
        handle
            .data(channel, pattern.slice(off..end))
            .await
            .map_err(|_| anyhow::anyhow!("channel closed by peer"))?;
        sent += (end - off) as u64;
    }

    handle.eof(channel).await.ok();
    handle.exit_status_request(channel, 0).await.ok();
    handle.close(channel).await.ok();
    let secs = started.elapsed().as_secs_f64();
    info!(
        mb = total / 1024 / 1024,
        secs, "stream finished ({:.1} MB/s)", total as f64 / secs / 1e6
    );
    Ok(())
}

/// 1MB 模式块：80 列带行号文本行，模拟日志/文本文件内容
fn build_pattern() -> bytes::Bytes {
    let mut v = Vec::with_capacity(1024 * 1024);
    let mut line_no: u32 = 0;
    while v.len() < 1024 * 1024 {
        line_no += 1;
        let line = format!(
            "{:08} The quick brown fox jumps over the lazy dog 0123456789abcdef\r\n",
            line_no
        );
        v.extend_from_slice(line.as_bytes());
    }
    bytes::Bytes::from(v)
}

/// sink：丢弃所有收到字节并计数（隧道上行压测对端）
async fn run_sink() -> Result<()> {
    let listener = TcpListener::bind(SINK_ADDR).await?;
    loop {
        let (mut sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        SINK_BYTES.fetch_add(n as u64, Ordering::Relaxed);
                    }
                }
            }
        });
    }
}

/// source：连接建立后全速发送（隧道下行压测对端）
async fn run_source() -> Result<()> {
    let listener = TcpListener::bind(SOURCE_ADDR).await?;
    loop {
        let (mut sock, _) = listener.accept().await?;
        tokio::spawn(async move {
            let buf = vec![0xABu8; 64 * 1024];
            loop {
                match sock.write(&buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        SOURCE_BYTES.fetch_add(n as u64, Ordering::Relaxed);
                    }
                }
            }
        });
    }
}

async fn report_stats() {
    let mut last_sink = 0u64;
    let mut last_source = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        let sink = SINK_BYTES.load(Ordering::Relaxed);
        let source = SOURCE_BYTES.load(Ordering::Relaxed);
        if sink != last_sink || source != last_source {
            info!(
                sink_mbps = (sink - last_sink) as f64 / 5.0 / 1e6,
                source_mbps = (source - last_source) as f64 / 5.0 / 1e6,
                "endpoint rates"
            );
        }
        last_sink = sink;
        last_source = source;
    }
}
