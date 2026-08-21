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
