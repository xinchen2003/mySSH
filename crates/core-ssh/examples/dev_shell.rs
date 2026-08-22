//! 开发冒烟用 SSH 服务端：接受任意 none/password 认证，PTY + shell 桥接到本地
//! powershell 子进程（管道 IO，非 ConPTY——仅验证 mySSH 全链路，不作他用）。
//!
//! 运行：cargo run -p core-ssh --example dev_shell -- [port]   （默认 2323）

use std::sync::Arc;

use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

struct DevShellServer;

struct DevShellHandler {
    stdin_tx: Option<mpsc::Sender<Vec<u8>>>,
    /// 持有 Child 句柄：局部变量在 shell_request 返回即 drop，
    /// kill_on_drop 会秒杀子进程（实测踩中）；handler 随连接存活正好对齐生命周期
    child: Option<tokio::process::Child>,
}

impl Server for DevShellServer {
    type Handler = DevShellHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> DevShellHandler {
        DevShellHandler {
            stdin_tx: None,
            child: None,
        }
    }
}

impl Handler for DevShellHandler {
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
        eprintln!("[dev_shell] shell_request ch={channel:?}");

        let mut child = tokio::process::Command::new("powershell.exe")
            .args(["-NoLogo", "-NoProfile"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(russh::Error::IO)?;
        eprintln!("[dev_shell] powershell spawned pid={:?}", child.id());

        let mut child_stdin = child.stdin.take().ok_or(russh::Error::SendError)?;
        let mut child_stdout = child.stdout.take().ok_or(russh::Error::SendError)?;
        let mut child_stderr = child.stderr.take().ok_or(russh::Error::SendError)?;
        // 句柄移交 handler 持有，随连接关闭被 kill_on_drop 回收
        self.child = Some(child);

        // 客户端输入 → 子进程 stdin
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(64);
        self.stdin_tx = Some(tx);
        tokio::spawn(async move {
            while let Some(data) = rx.recv().await {
                if child_stdin.write_all(&data).await.is_err() {
                    break;
                }
            }
        });

        // 子进程 stdout/stderr → 客户端通道（PTY 语义：合并渲染）
        let handle = session.handle();
        let err_handle = handle.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            loop {
                match child_stderr.read(&mut buf).await {
                    Ok(n) if n > 0 => {
                        if err_handle.data(channel, buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });
        tokio::spawn(async move {
            let mut buf = vec![0u8; 32768];
            loop {
                match child_stdout.read(&mut buf).await {
                    Ok(0) => {
                        eprintln!("[dev_shell] child stdout EOF");
                        let _ = handle.eof(channel).await;
                        let _ = handle.close(channel).await;
                        break;
                    }
                    Ok(n) => {
                        if handle.data(channel, buf[..n].to_vec()).await.is_err() {
                            eprintln!("[dev_shell] handle.data failed");
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Ok(())
    }

    async fn data(
        &mut self,
        _channel: ChannelId,
        data: &[u8],
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(tx) = &self.stdin_tx {
            let _ = tx.send(data.to_vec()).await;
        }
        Ok(())
    }
}

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(2323);
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .unwrap_or_else(|e| panic!("keygen: {e}"));
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);
    methods.push(MethodKind::Password);
    let config = russh::server::Config {
        methods,
        keys: vec![key],
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap_or_else(|e| panic!("bind: {e}"));
    println!("dev_shell listening on 127.0.0.1:{port}");
    let mut server = DevShellServer;
    let _ = server.run_on_socket(Arc::new(config), &listener).await;
}
