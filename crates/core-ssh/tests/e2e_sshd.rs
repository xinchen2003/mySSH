//! 真机 sshd E2E（轴一 1.3）：CI 起 OpenSSH 容器后运行。
//! 本地无环境变量时全部跳过（不影响 `cargo test --workspace`）。
//!
//! 环境变量：MYSSH_E2E_HOST（必需，缺省即跳过）、MYSSH_E2E_PORT（默认 22）、
//! MYSSH_E2E_USER / MYSSH_E2E_PASSWORD（密码认证）、MYSSH_E2E_SU_PASSWORD（su 用例）。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, HostKeyDecision, HostKeyPrompt,
    KeepaliveConfig, KnownHostsPolicy, SshConnection,
};
use zeroize::Zeroizing;

struct E2e {
    host: String,
    port: u16,
    user: String,
    password: String,
}

/// 无环境即 None → 测试原地跳过
fn e2e() -> Option<E2e> {
    let host = std::env::var("MYSSH_E2E_HOST").ok()?;
    Some(E2e {
        host,
        port: std::env::var("MYSSH_E2E_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(22),
        user: std::env::var("MYSSH_E2E_USER").unwrap_or_else(|_| "tester".into()),
        password: std::env::var("MYSSH_E2E_PASSWORD").expect("MYSSH_E2E_PASSWORD 未设置"),
    })
}

fn opts(e: &E2e, host_key_check: HostKeyCheck) -> ConnectOptions {
    ConnectOptions {
        host: e.host.clone(),
        port: e.port,
        user: e.user.clone(),
        auth: AuthMethod::Password(Zeroizing::new(e.password.clone())),
        jump_chain: vec![],
        class: ConnClass::Interactive,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        host_key_check,
        ki_prompter: None,
    }
}

/// 读 PTY 输出直到出现 marker（或 15s 超时）
async fn read_until(reader: &mut core_ssh::PtyReader, marker: &str) -> String {
    let mut buf = String::new();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !buf.contains(marker) {
            match reader.next_data().await {
                Some(chunk) => buf.push_str(&String::from_utf8_lossy(&chunk)),
                None => break,
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("15s 内未等到 marker {marker:?}；已收到: {buf:?}"));
    buf
}

#[tokio::test]
async fn pty_typing_echo_roundtrip() {
    let Some(e) = e2e() else { return };
    let conn = SshConnection::connect(opts(&e, HostKeyCheck::AcceptAll))
        .await
        .expect("连接失败");
    let pty = conn
        .open_pty("xterm", 80, 24, None)
        .await
        .expect("开 PTY 失败");
    let (mut reader, writer) = pty.split();
    // 连接→键入→回显：marker 由远端 shell 实际求值回显
    writer.write(b"echo E2E_$((6*7))\r").await.unwrap();
    let out = read_until(&mut reader, "E2E_42").await;
    assert!(out.contains("E2E_42"), "回显缺 marker: {out:?}");
}

#[tokio::test]
async fn exec_collect_roundtrip() {
    let Some(e) = e2e() else { return };
    let conn = SshConnection::connect(opts(&e, HostKeyCheck::AcceptAll))
        .await
        .expect("连接失败");
    let out = conn
        .exec_collect("printf hello-e2e")
        .await
        .expect("exec 失败");
    assert_eq!(out, b"hello-e2e");
}

#[tokio::test]
async fn known_hosts_first_learn_then_changed_reject() {
    let Some(e) = e2e() else { return };
    let dir = std::env::temp_dir().join(format!("myssh-e2e-kh-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let kh = dir.join("known_hosts");

    // 首连：prompter 应收到 Unknown → Learn 学入
    let prompts: Arc<Mutex<Vec<HostKeyPrompt>>> = Arc::new(Mutex::new(vec![]));
    let p2 = prompts.clone();
    let check = HostKeyCheck::KnownHosts(KnownHostsPolicy {
        path: kh.clone(),
        prompter: Arc::new(move |prompt: HostKeyPrompt| {
            p2.lock().push(prompt);
            async move { HostKeyDecision::Learn }
        }),
    });
    SshConnection::connect(opts(&e, check))
        .await
        .expect("首连失败");
    {
        let seen = prompts.lock();
        assert_eq!(seen.len(), 1, "首连应收一次 Unknown 提示");
        assert!(
            matches!(seen[0], HostKeyPrompt::Unknown { .. }),
            "首连提示类型应为 Unknown: {:?}",
            seen[0]
        );
    }

    // 二连：记录已学入，prompter 不再触发
    SshConnection::connect(opts(
        &e,
        check_host(kh.clone(), prompts.clone(), HostKeyDecision::Reject),
    ))
    .await
    .expect("二次连接应免提示直连");
    assert_eq!(prompts.lock().len(), 1, "已学入主机不应再提示");

    // 同类型不同密钥改写记录 → Changed 提示；Reject 后连接必须失败。
    // 用新生成的合法密钥（而非篡改 base64）保证 russh 解析路径稳定命中 KeyChanged。
    {
        use russh::keys::{Algorithm, PrivateKey, PublicKeyBase64};
        let raw = std::fs::read_to_string(&kh).unwrap();
        let mut parts = raw.split_whitespace();
        let hostpat = parts.next().unwrap().to_string();
        let ktype = parts.next().unwrap().to_string();
        let alg = match ktype.as_str() {
            "ssh-ed25519" => Algorithm::Ed25519,
            "ecdsa-sha2-nistp256" => Algorithm::Ecdsa {
                curve: russh::keys::EcdsaCurve::NistP256,
            },
            "ssh-rsa" => Algorithm::Rsa {
                hash: Some(russh::keys::HashAlg::Sha256),
            },
            other => panic!("容器未预期的主机密钥类型: {other}"),
        };
        let fake = PrivateKey::random(&mut rand::rng(), alg)
            .unwrap()
            .public_key()
            .clone();
        std::fs::write(
            &kh,
            format!("{hostpat} {ktype} {}\n", fake.public_key_base64()),
        )
        .unwrap();
    }
    let p3 = prompts.clone();
    let check = HostKeyCheck::KnownHosts(KnownHostsPolicy {
        path: kh.clone(),
        prompter: Arc::new(move |prompt: HostKeyPrompt| {
            p3.lock().push(prompt);
            async move { HostKeyDecision::Reject }
        }),
    });
    let r = SshConnection::connect(opts(&e, check)).await;
    let seen = prompts.lock();
    assert!(
        seen.iter()
            .any(|p| matches!(p, HostKeyPrompt::Changed { .. })),
        "篡改后应收 Changed 提示: {seen:?}"
    );
    assert!(r.is_err(), "Changed + Reject 必须拒绝连接");

    let _ = std::fs::remove_dir_all(&dir);
}

fn check_host(
    path: std::path::PathBuf,
    prompts: Arc<Mutex<Vec<HostKeyPrompt>>>,
    decision: HostKeyDecision,
) -> HostKeyCheck {
    HostKeyCheck::KnownHosts(KnownHostsPolicy {
        path,
        prompter: Arc::new(move |prompt: HostKeyPrompt| {
            prompts.lock().push(prompt);
            async move { decision }
        }),
    })
}

/// su 密码应答链路（协议级）：su - root 触发密码提示，应答后落到 root shell。
/// 对应 app 层 SuWatch 依赖的真实 PAM 提示形态；锁定「提示出现→应答→提权成功」。
#[tokio::test]
async fn su_password_prompt_answer() {
    let (Some(e), Ok(su_pw)) = (e2e(), std::env::var("MYSSH_E2E_SU_PASSWORD")) else {
        return;
    };
    let conn = SshConnection::connect(opts(&e, HostKeyCheck::AcceptAll))
        .await
        .expect("连接失败");
    let pty = conn
        .open_pty("xterm", 80, 24, None)
        .await
        .expect("开 PTY 失败");
    let (mut reader, writer) = pty.split();

    writer.write(b"su - root\r").await.unwrap();
    let out = read_until(&mut reader, "assword").await;
    assert!(out.contains("assword"), "未出现密码提示: {out:?}");

    writer.write(format!("{su_pw}\r").as_bytes()).await.unwrap();
    writer.write(b"id -u\r").await.unwrap();
    let out = read_until(&mut reader, "\r\n0\r\n").await;
    assert!(out.contains("\r\n0\r\n"), "su 后 uid 应为 0: {out:?}");
}
