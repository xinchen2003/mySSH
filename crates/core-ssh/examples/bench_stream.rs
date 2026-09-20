//! 吞吐基准 SSH 服务端（PR-6 验收：高速 cat 对照用）。
//! 接受任意 none/password 认证；PTY+shell 一旦建立即全速下灌 N MiB 编号行，
//! 末尾追加唯一完成标记行，随后 EOF + 关闭通道。
//!
//! 运行：cargo run -p core-ssh --example bench_stream -- [port] [totalMiB]
//! （默认 127.0.0.1:2324 / 100 MiB）

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::net::TcpListener;

/// 单次下灌块大小（贴近终端 read 侧常见 32KB）
const CHUNK: usize = 32 * 1024;
/// 前端判定端到端完成的唯一标记（出现在滚动缓冲末尾即说明全量已渲染）
const DONE_MARKER: &str = "bench-stream-done";

struct BenchServer {
    total: u64,
}

struct BenchHandler {
    total: u64,
}

impl Server for BenchServer {
    type Handler = BenchHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> BenchHandler {
        BenchHandler { total: self.total }
    }
}

impl Handler for BenchHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
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

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        _col: u32,
        _row: u32,
        _pw: u32,
        _ph: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        let handle = session.handle();
        let total = self.total;
        tokio::spawn(async move {
            // 32KB 编号行块：行内容逐行递增，杜绝任何压缩/去重捷径
            let mut chunk = Vec::with_capacity(CHUNK + 64);
            let mut line_no = 0u64;
            while chunk.len() < CHUNK {
                chunk.extend_from_slice(
                    format!("bench-stream line {line_no:08} xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\r\n")
                        .as_bytes(),
                );
                line_no += 1;
            }
            // Handle::data 要求 'static 负载：Bytes 共享所有权，slice 复制仅改指针
            let chunk = bytes::Bytes::from(chunk);
            let mut sent = 0u64;
            while sent < total {
                let n = (total - sent).min(chunk.len() as u64) as usize;
                if handle.data(channel, chunk.slice(..n)).await.is_err() {
                    return;
                }
                sent += n as u64;
            }
            let trailer = bytes::Bytes::from(format!("{DONE_MARKER} bytes={sent}\r\n"));
            let _ = handle.data(channel, trailer).await;
            let _ = handle.eof(channel).await;
            let _ = handle.close(channel).await;
        });
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2324);
    let mib: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100);
    let key = match PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("生成主机密钥失败: {e}");
            std::process::exit(1);
        }
    };
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);
    methods.push(MethodKind::Password);
    let config = russh::server::Config {
        methods,
        keys: vec![key],
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = match TcpListener::bind(("127.0.0.1", port)).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("绑定 127.0.0.1:{port} 失败: {e}");
            std::process::exit(1);
        }
    };
    println!("bench_stream 监听 127.0.0.1:{port}，每连接下灌 {mib} MiB + 完成标记后 EOF");
    let mut server = BenchServer {
        total: mib * 1024 * 1024,
    };
    if let Err(e) = server.run_on_socket(Arc::new(config), &listener).await {
        eprintln!("服务端退出: {e}");
        std::process::exit(1);
    }
}
