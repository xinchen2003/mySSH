//! PR-18 性能门禁（PR 级微基准）：TransferQueue 调度 + SFTP 读写回环。
//!
//! 全自包含：进程内极简 russh-sftp 服务端（真实文件读写，临时目录）+
//! TransferQueue（SFTP_EXEC 执行槽）跑 N 个上传 + N 个下载，
//! 输出单任务 median 延迟与聚合吞吐，超阈值退出码非零。
//!
//!   cargo run --release -p core-sftp --example gate_sftp
//!
//! 阈值口径同 gate_relay：本机首测 median × 约 2 倍裕度。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use std::time::Instant;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, Msg, Server as SshServer, Session};
use russh::{ChannelId, MethodKind, MethodSet};
use russh_sftp::protocol::{
    File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

const TASKS: usize = 40;
const FILE_SIZE: usize = 256 * 1024;
/// 阈值（本机 release 首测 median 134ms/任务、18MiB/s；回归探测取观测值约一半/数倍裕度）
const MEDIAN_TASK_MS_MAX: f64 = 400.0;
const THROUGHPUT_MIBS_MIN: f64 = 9.0;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

// ---- 极简 SFTP 服务端（仅传输所需：open/read/write/close/stat/fstat）----

struct Fs {
    root: PathBuf,
    handles: Mutex<HashMap<String, PathBuf>>,
    seq: AtomicU64,
}

impl Fs {
    fn resolve(&self, p: &str) -> Result<PathBuf, StatusCode> {
        let rel = p.trim_start_matches('/');
        if rel.contains("..") {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(self.root.join(rel))
    }

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }
}

fn attrs_of(meta: &std::fs::Metadata) -> FileAttributes {
    FileAttributes {
        size: Some(meta.len()),
        ..Default::default()
    }
}

impl russh_sftp::server::Handler for Fs {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let path = self.resolve(&filename)?;
        if pflags.contains(OpenFlags::CREATE) {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if pflags.contains(OpenFlags::TRUNCATE) {
                let _ = std::fs::write(&path, b"");
            } else {
                let _ = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path);
            }
        }
        let meta = std::fs::metadata(&path).map_err(|_| StatusCode::NoSuchFile)?;
        if !pflags.contains(OpenFlags::WRITE) && meta.is_dir() {
            return Err(StatusCode::PermissionDenied);
        }
        let h = format!("h{}", self.seq.fetch_add(1, Ordering::Relaxed));
        self.handles.lock().insert(h.clone(), path);
        Ok(Handle { id, handle: h })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.handles.lock().remove(&handle);
        Ok(Self::ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<russh_sftp::protocol::Data, Self::Error> {
        let path = self
            .handles
            .lock()
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        let mut f = tokio::fs::File::open(&path)
            .await
            .map_err(|_| StatusCode::Failure)?;
        f.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| StatusCode::Failure)?;
        let mut buf = vec![0u8; len as usize];
        let n = f.read(&mut buf).await.map_err(|_| StatusCode::Failure)?;
        buf.truncate(n);
        Ok(russh_sftp::protocol::Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let path = self
            .handles
            .lock()
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        let mut f = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .await
            .map_err(|_| StatusCode::Failure)?;
        f.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| StatusCode::Failure)?;
        f.write_all(&data).await.map_err(|_| StatusCode::Failure)?;
        Ok(Self::ok(id))
    }

    async fn stat(
        &mut self,
        id: u32,
        path: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let p = self.resolve(&path)?;
        let meta = std::fs::metadata(&p).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(russh_sftp::protocol::Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn fstat(
        &mut self,
        id: u32,
        handle: String,
    ) -> Result<russh_sftp::protocol::Attrs, Self::Error> {
        let path = self
            .handles
            .lock()
            .get(&handle)
            .cloned()
            .ok_or(StatusCode::Failure)?;
        let meta = std::fs::metadata(path).map_err(|_| StatusCode::Failure)?;
        Ok(russh_sftp::protocol::Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        _path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(Self::ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let p = self.resolve(&path)?;
        if !p.is_dir() {
            return Err(StatusCode::NoSuchFile);
        }
        Ok(Handle {
            id,
            handle: format!("d{}", self.seq.fetch_add(1, Ordering::Relaxed)),
        })
    }

    async fn readdir(&mut self, id: u32, _handle: String) -> Result<Name, Self::Error> {
        // 门禁只跑单文件传输，目录列举返回空集即 EOF
        Ok(Name {
            id,
            files: vec![File::dummy(".".to_string())],
        })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let p = self.resolve(&filename)?;
        let _ = std::fs::remove_file(p);
        Ok(Self::ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let p = self.resolve(&path).map_err(|_| StatusCode::Failure)?;
        let _ = std::fs::create_dir_all(p);
        Ok(Self::ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::dummy(path)],
        })
    }
}

struct GateSsh {
    root: PathBuf,
}

struct GateSshHandler {
    root: PathBuf,
    channels: tokio::sync::Mutex<HashMap<ChannelId, russh::Channel<Msg>>>,
}

impl SshServer for GateSsh {
    type Handler = GateSshHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> GateSshHandler {
        GateSshHandler {
            root: self.root.clone(),
            channels: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl russh::server::Handler for GateSshHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: russh::Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.channels.lock().await.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            let Some(channel) = self.channels.lock().await.remove(&channel_id) else {
                return session.channel_failure(channel_id);
            };
            let root = self.root.clone();
            session.channel_success(channel_id)?;
            tokio::spawn(async move {
                russh_sftp::server::run(
                    channel.into_stream(),
                    Fs {
                        root,
                        handles: Mutex::new(HashMap::new()),
                        seq: AtomicU64::new(1),
                    },
                )
                .await;
            });
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

async fn start_server(root: PathBuf) -> u16 {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    let mut methods = MethodSet::empty();
    methods.push(MethodKind::None);
    let config = russh::server::Config {
        methods,
        keys: vec![key],
        window_size: 8 * 1024 * 1024,
        maximum_packet_size: 32768,
        nodelay: true,
        inactivity_timeout: None,
        ..Default::default()
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = GateSsh { root }
            .run_on_socket(Arc::new(config), &listener)
            .await;
    });
    port
}

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("myssh-gate-sftp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn connect(port: u16) -> SshConnection {
    SshConnection::connect(ConnectOptions {
        host: "127.0.0.1".into(),
        port,
        user: "gate".into(),
        auth: AuthMethod::None,
        class: ConnClass::Bulk,
        window_size: 8 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        jump_chain: vec![],
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    })
    .await
    .expect("connect")
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let remote_root = temp_root("remote");
    let port = start_server(remote_root.clone()).await;
    let conn = connect(port).await;
    let slot =
        core_sftp::SftpSlot::open_sftp("gate", Arc::new(conn), tokio::runtime::Handle::current())
            .await
            .expect("sftp slot");
    let q = Arc::new(core_sftp::TransferQueue::new(
        slot,
        core_policy::budget::caps::SFTP_EXEC,
        tokio::runtime::Handle::current(),
    ));

    // 本地源文件
    let local_root = temp_root("local");
    let payload = vec![0xcdu8; FILE_SIZE];
    for i in 0..TASKS {
        std::fs::write(local_root.join(format!("up{i}.bin")), &payload).unwrap();
    }

    let start = Instant::now();
    let mut lat = Vec::with_capacity(TASKS * 2);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(Instant, core_sftp::TransferState)>(TASKS * 2);
    let t0 = Instant::now();
    q.set_progress_callback(Arc::new(move |info| {
        if matches!(
            info.state,
            core_sftp::TransferState::Done
                | core_sftp::TransferState::Failed
                | core_sftp::TransferState::Canceled
        ) {
            let _ = tx.try_send((t0, info.state));
        }
    }));

    let mut ids = Vec::new();
    for i in 0..TASKS {
        let t0 = Instant::now();
        lat.push(t0);
        ids.push(
            q.enqueue_upload(
                local_root.join(format!("up{i}.bin")),
                format!("/up{i}.bin"),
                FILE_SIZE as u64,
                core_sftp::OnExists::Overwrite,
            )
            .await,
        );
    }
    for i in 0..TASKS {
        lat.push(Instant::now());
        ids.push(
            q.enqueue_download(
                format!("/up{i}.bin"),
                local_root.join(format!("down{i}.bin")),
                FILE_SIZE as u64,
                core_sftp::OnExists::Overwrite,
            )
            .await,
        );
    }

    let mut done = 0usize;
    let mut per_task: Vec<f64> = Vec::new();
    while done < TASKS * 2 {
        let Some((queued_at, state)) = rx.recv().await else {
            break;
        };
        assert!(
            state == core_sftp::TransferState::Done,
            "传输必须全部成功: {state:?}"
        );
        per_task.push(queued_at.elapsed().as_secs_f64() * 1000.0);
        done += 1;
    }
    let wall_s = start.elapsed().as_secs_f64();
    let med = median(per_task);
    let mibs = (TASKS * 2 * FILE_SIZE) as f64 / 1024.0 / 1024.0 / wall_s;

    // 抽查一致性：下载必须逐字节等于上传源
    assert_eq!(
        std::fs::read(local_root.join("down0.bin")).unwrap(),
        payload,
        "回环内容必须一致"
    );

    let ok_med = med <= MEDIAN_TASK_MS_MAX;
    let ok_tput = mibs >= THROUGHPUT_MIBS_MIN;
    println!("GATE sftp.medianTaskMs value={med:.1} threshold={MEDIAN_TASK_MS_MAX} ok={ok_med}");
    println!(
        "GATE sftp.throughputMiBs value={mibs:.0} threshold={THROUGHPUT_MIBS_MIN} ok={ok_tput}"
    );
    let _ = ids;
    if !(ok_med && ok_tput) {
        eprintln!("gate_sftp 未过门禁");
        std::process::exit(1);
    }
}
