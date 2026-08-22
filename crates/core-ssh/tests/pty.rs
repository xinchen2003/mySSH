//! M1 测试：PTY 通道（写/读/resize）、publickey 认证、KI 直通、known_hosts 首连学习。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, HostKeyPrompt, KeepaliveConfig,
    KnownHostsPolicy, SshConnection,
};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use tokio::net::TcpListener;

/// PTY 服务端共享观测状态
#[derive(Default)]
struct Observed {
    pty: Option<(String, u32, u32)>,
    resizes: Vec<(u32, u32)>,
}

#[derive(Clone)]
struct PtyServer {
    observed: Arc<Mutex<Observed>>,
}

struct PtyHandler {
    observed: Arc<Mutex<Observed>>,
}

impl Server for PtyServer {
    type Handler = PtyHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> PtyHandler {
        PtyHandler {
            observed: self.observed.clone(),
        }
    }
}

impl Handler for PtyHandler {
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

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.observed.lock().pty = Some((term.to_string(), col_width, row_height));
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // echo：把客户端输入原样写回
        session
            .handle()
            .data(channel, data.to_vec())
            .await
            .map_err(|_| russh::Error::SendError)?;
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.observed.lock().resizes.push((col_width, row_height));
        Ok(())
    }
}

async fn start_pty_server() -> (u16, Arc<Mutex<Observed>>) {
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
    let observed = Arc::new(Mutex::new(Observed::default()));
    let srv = PtyServer {
        observed: observed.clone(),
    };
    tokio::spawn(async move {
        let mut server = srv;
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });
    (port, observed)
}

fn test_opts(port: u16, auth: AuthMethod) -> ConnectOptions {
    ConnectOptions {
        host: "127.0.0.1".into(),
        port,
        user: "test".into(),
        auth,
        class: ConnClass::Interactive,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    }
}

#[tokio::test]
async fn pty_write_echo_and_resize() {
    let (port, observed) = start_pty_server().await;
    let conn = SshConnection::connect(test_opts(port, AuthMethod::None))
        .await
        .expect("connect");

    let pty = conn
        .open_pty("xterm-256color", 120, 40, None)
        .await
        .expect("open pty");
    let (mut reader, writer) = pty.split();

    // 服务端应已收到 PTY 请求与维度
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if observed.lock().pty.is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "pty_request not observed"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        observed.lock().pty.clone(),
        Some(("xterm-256color".to_string(), 120, 40))
    );

    // 写入 → echo 读回
    writer.write(b"hello-myssh").await.expect("write");
    let data = tokio::time::timeout(Duration::from_secs(2), reader.next_data())
        .await
        .expect("read timeout")
        .expect("stream ended early");
    assert_eq!(&data[..], b"hello-myssh");

    // resize → 服务端收到 window_change
    writer.resize(132, 50).await.expect("resize");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if observed.lock().resizes.contains(&(132, 50)) {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "resize not observed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    writer.close().await.expect("close");
}

#[tokio::test]
async fn pty_exec_command() {
    let (port, _observed) = start_pty_server().await;
    let conn = SshConnection::connect(test_opts(port, AuthMethod::None))
        .await
        .expect("connect");
    // command 变体：走 exec 而非 shell（服务端 data echo 不受请求类型影响）
    let pty = conn
        .open_pty("xterm-256color", 80, 24, Some("top"))
        .await
        .expect("open pty with command");
    let (mut reader, writer) = pty.split();
    writer.write(b"ping").await.expect("write");
    let data = tokio::time::timeout(Duration::from_secs(2), reader.next_data())
        .await
        .expect("read timeout")
        .expect("stream ended early");
    assert_eq!(&data[..], b"ping");
}

/// publickey 认证服务端：接受任意公钥
struct PkServer;
struct PkHandler;

impl Server for PkServer {
    type Handler = PkHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> PkHandler {
        PkHandler
    }
}

impl Handler for PkHandler {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }
}

#[tokio::test]
async fn publickey_auth_roundtrip() {
    let server_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::PublicKey);
    let config = russh::server::Config {
        methods,
        keys: vec![server_key],
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut server = PkServer;
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });

    // 客户端生成 ed25519 私钥，OpenSSH 格式文本走 AuthMethod::PublicKey
    let client_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let key_pem = client_key
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .expect("serialize key");
    let opts = test_opts(
        port,
        AuthMethod::PublicKey {
            key_pem,
            passphrase: None,
        },
    );
    SshConnection::connect(opts)
        .await
        .expect("publickey auth must succeed");
}

/// KI 直通：服务端立即 Accept（无质询轮），验证 start→Success 路径
struct KiServer;
struct KiHandler;

impl Server for KiServer {
    type Handler = KiHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> KiHandler {
        KiHandler
    }
}

impl Handler for KiHandler {
    type Error = russh::Error;

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        _response: Option<russh::server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }
}

#[tokio::test]
async fn keyboard_interactive_immediate_success() {
    let server_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::KeyboardInteractive);
    let config = russh::server::Config {
        methods,
        keys: vec![server_key],
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut server = KiServer;
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });

    let mut opts = test_opts(port, AuthMethod::KeyboardInteractive);
    // 服务端直通时 prompter 不应被调用；给了也得能成功
    opts.ki_prompter = Some(Arc::new(|_challenge| async {
        panic!("prompter must not be called on immediate accept")
    }));
    SshConnection::connect(opts)
        .await
        .expect("keyboard-interactive immediate accept must succeed");
}

/// known_hosts 首连：Unknown 弹窗 → Learn → 写入文件；二次连接不再弹窗
#[tokio::test]
async fn known_hosts_first_connect_learn_then_trusted() {
    let (port, _observed) = start_pty_server().await;
    let dir = std::env::temp_dir().join(format!("myssh-kh-test-{}-{}", std::process::id(), port));
    std::fs::create_dir_all(&dir).unwrap();
    let kh_path = dir.join("known_hosts");

    let prompts: Arc<Mutex<Vec<HostKeyPrompt>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = prompts.clone();
    let prompter = move |p: HostKeyPrompt| {
        recorded.lock().push(p);
        async move { core_ssh::HostKeyDecision::Learn }
    };

    let mut opts = test_opts(port, AuthMethod::None);
    opts.host_key_check = HostKeyCheck::KnownHosts(KnownHostsPolicy {
        path: kh_path.clone(),
        prompter: Arc::new(prompter.clone()),
    });
    SshConnection::connect(opts).await.expect("first connect");

    {
        let log = prompts.lock();
        assert_eq!(log.len(), 1, "first connect must prompt once");
        match &log[0] {
            HostKeyPrompt::Unknown {
                host,
                port: p,
                fingerprint,
                ..
            } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(*p, port);
                assert!(fingerprint.starts_with("SHA256:"), "fingerprint format");
            }
            other => panic!("expected Unknown prompt, got {other:?}"),
        }
    }
    assert!(kh_path.exists(), "known_hosts must be written");

    // 二次连接：已信任，不再弹窗
    let mut opts2 = test_opts(port, AuthMethod::None);
    opts2.host_key_check = HostKeyCheck::KnownHosts(KnownHostsPolicy {
        path: kh_path.clone(),
        prompter: Arc::new(prompter),
    });
    SshConnection::connect(opts2).await.expect("second connect");
    assert_eq!(
        prompts.lock().len(),
        1,
        "trusted host must not prompt again"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// agent 回环：russh agent server（命名管道）+ add_identity → AuthMethod::Agent 建连。
/// 两个断言共用一个测试体串行执行——MYSSH_AGENT_PIPE 是进程级环境变量，避免并发互踩。
#[cfg(windows)]
#[tokio::test]
async fn agent_auth_roundtrip_and_unavailable() {
    use russh::keys::agent::client::AgentClient;
    use russh::keys::agent::server as agent_server;

    // 1. agent server 监听测试管道（futures mpsc Receiver 原生实现 Stream）。
    // 首个实例同步创建：命名管道必须先存在，客户端 connect 才不会吃 NotFound
    let pipe_path = format!(r"\\.\pipe\myssh-test-agent-{}", std::process::id());
    let first = tokio::net::windows::named_pipe::ServerOptions::new()
        .create(&pipe_path)
        .expect("create pipe");
    let (mut tx, rx) = futures::channel::mpsc::channel::<
        std::io::Result<tokio::net::windows::named_pipe::NamedPipeServer>,
    >(8);
    let accept_path = pipe_path.clone();
    tokio::spawn(async move {
        use futures::SinkExt;
        let mut pending = Some(first);
        loop {
            let server = match pending.take() {
                Some(s) => s,
                None => match tokio::net::windows::named_pipe::ServerOptions::new()
                    .create(&accept_path)
                {
                    Ok(s) => s,
                    Err(_) => break,
                },
            };
            let _ = server.connect().await;
            if tx.send(Ok(server)).await.is_err() {
                break;
            }
        }
    });
    tokio::spawn(agent_server::serve(rx, ()));

    // 2. 客户端私钥注入 agent
    let client_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut agent_client = AgentClient::connect_named_pipe(&pipe_path)
        .await
        .expect("connect agent pipe");
    agent_client
        .add_identity(&client_key, &[])
        .await
        .expect("add_identity");

    // 3. SSH 服务端（签名校验由 russh 协议层完成，auth_publickey 接受即可）
    let server_key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::PublicKey);
    let config = russh::server::Config {
        methods,
        keys: vec![server_key],
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut server = PkServer;
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });

    // 4. AuthMethod::Agent 走 MYSSH_AGENT_PIPE → 应建连成功
    std::env::set_var("MYSSH_AGENT_PIPE", &pipe_path);
    let opts = test_opts(port, AuthMethod::Agent);
    SshConnection::connect(opts)
        .await
        .expect("agent auth must succeed");

    // 5. 负路径：管道不存在 → E2 凭据不可用
    std::env::set_var("MYSSH_AGENT_PIPE", r"\\.\pipe\myssh-nonexistent-agent");
    let opts = test_opts(port, AuthMethod::Agent);
    let err = match SshConnection::connect(opts).await {
        Ok(_) => panic!("agent auth must fail when pipe missing"),
        Err(e) => e,
    };
    assert!(
        err.to_string().starts_with("E2"),
        "missing agent must be E2-coded: {err}"
    );

    std::env::remove_var("MYSSH_AGENT_PIPE");
}
