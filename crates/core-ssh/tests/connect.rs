//! 冒烟测试：内存中起 russh 服务端，验证 core-ssh 的连接 + exec 链路。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::net::TcpListener;

/// 最小 echo 服务端：none 认证 + `echo` exec 回显
struct TestServer;
struct TestHandler;

impl Server for TestServer {
    type Handler = TestHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> TestHandler {
        TestHandler
    }
}

impl Handler for TestHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    /// 跳板测试需要：direct-tcpip 真实桥接到目标（与 dev_shell 同构）
    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        match tokio::net::TcpStream::connect((host_to_connect, port_to_connect as u16)).await {
            Ok(tcp) => {
                reply.accept().await;
                tokio::spawn(bridge(channel, tcp));
            }
            Err(_) => {
                let _ = reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            }
        }
        Ok(())
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
        if data == b"echo-test" {
            session.channel_success(channel)?;
            let handle = session.handle();
            tokio::spawn(async move {
                let _ = handle.data(channel, &b"pong"[..]).await;
                let _ = handle.eof(channel).await;
                let _ = handle.close(channel).await;
            });
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }
}

/// TCP <-> SSH 通道双向泵
async fn bridge(ch: Channel<Msg>, tcp: tokio::net::TcpStream) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let chan = ch.into_stream();
    let (mut cr, mut cw) = tokio::io::split(chan);
    let (mut tr, mut tw) = tcp.into_split();
    let up = tokio::spawn(async move {
        let mut buf = vec![0u8; 32768];
        loop {
            match tr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if cw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let down = tokio::spawn(async move {
        let mut buf = vec![0u8; 32768];
        loop {
            match cr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tw.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    let _ = tokio::join!(up, down);
}

async fn start_server() -> u16 {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);
    let config = russh::server::Config {
        methods,
        keys: vec![key],
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut server = TestServer;
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });
    port
}

#[tokio::test]
async fn connect_and_echo() {
    let port = start_server().await;
    let conn = SshConnection::connect(ConnectOptions {
        host: "127.0.0.1".into(),
        port,
        user: "test".into(),
        auth: AuthMethod::None,
        class: ConnClass::Interactive,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        jump_chain: vec![],
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    })
    .await
    .expect("connect");

    let out = conn.exec_collect("echo-test").await.expect("exec");
    assert_eq!(out, b"pong");
}

#[tokio::test]
async fn auth_failure_is_coded() {
    let port = start_server().await;
    // 服务端只接受 none；password 应得 E2 错误
    let opts = ConnectOptions {
        host: "127.0.0.1".into(),
        port,
        user: "test".into(),
        auth: AuthMethod::Password(zeroize::Zeroizing::new("wrong".into())),
        class: ConnClass::Bulk,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        jump_chain: vec![],
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    };
    let err = match SshConnection::connect(opts).await {
        Ok(_) => panic!("password auth must fail against none-only server"),
        Err(e) => e,
    };
    let msg = err.to_string();
    assert!(msg.starts_with("E2"), "error must carry E2 code: {msg}");
}

/// ProxyJump：目标 B 仅经跳板 A 的 direct-tcpip 到达（双跳握手 + exec 回显）
#[tokio::test]
async fn jump_chain_connect() {
    let target_port = start_server().await;
    let hop_port = start_server().await; // 同一 TestServer 带 direct-tcpip 桥
    let conn = SshConnection::connect(ConnectOptions {
        host: "127.0.0.1".into(),
        port: target_port,
        user: "test".into(),
        auth: AuthMethod::None,
        class: ConnClass::Interactive,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        jump_chain: vec![core_ssh::JumpHop {
            host: "127.0.0.1".into(),
            port: hop_port,
            user: "hop".into(),
            auth: AuthMethod::None,
        }],
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    })
    .await
    .expect("connect via jump");
    let out = conn.exec_collect("echo-test").await.expect("exec");
    assert_eq!(out, b"pong");
}
