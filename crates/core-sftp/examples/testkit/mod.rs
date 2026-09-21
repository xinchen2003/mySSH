//! core-sftp examples 共享测试服务端（门禁与基准共用）。
//!
//! 注意：examples/ 下每个 .rs 独立成 crate，本文件经
//! `#[path = "testkit/mod.rs"] mod testkit;` 引入，cargo 不单独编译。
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use parking_lot::Mutex;
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, Msg, Server as SshServer, Session};
use russh::{ChannelId, MethodKind, MethodSet};
use russh_sftp::protocol::{
    File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

pub fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[v.len() / 2]
}

pub fn p95(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    v[((v.len() as f64) * 0.95) as usize]
}
// ---- 极简 SFTP 服务端（仅传输所需：open/read/write/close/stat/fstat）----
// delay_ms：每请求人造延迟，用于在本机模拟 RTT（基准用；门禁为 0）

pub struct Fs {
    pub root: PathBuf,
    pub handles: Mutex<HashMap<String, PathBuf>>,
    /// 目录句柄 → 待返回条目（readdir 首批全给、再给空 = EOF）
    pub dirs: Mutex<HashMap<String, Vec<File>>>,
    pub seq: AtomicU64,
    pub delay_ms: Arc<AtomicU64>,
}

impl Fs {
    pub fn new(root: PathBuf, delay_ms: Arc<AtomicU64>) -> Self {
        Self {
            root,
            handles: Mutex::new(HashMap::new()),
            dirs: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(1),
            delay_ms,
        }
    }

    pub fn resolve(&self, p: &str) -> Result<PathBuf, StatusCode> {
        let rel = p.trim_start_matches('/');
        if rel.contains("..") {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(self.root.join(rel))
    }

    pub fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }
}

pub fn attrs_of(meta: &std::fs::Metadata) -> FileAttributes {
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
        let delay = self.delay_ms.load(Ordering::Relaxed);
        if delay > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }
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
        let mut files = Vec::new();
        let mut rd = tokio::fs::read_dir(&p)
            .await
            .map_err(|_| StatusCode::Failure)?;
        while let Ok(Some(e)) = rd.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            let meta = e
                .metadata()
                .await
                .unwrap_or_else(|_| std::fs::metadata(".").unwrap());
            files.push(File::new(name, attrs_of(&meta)));
        }
        let h = format!("d{}", self.seq.fetch_add(1, Ordering::Relaxed));
        self.dirs.lock().insert(h.clone(), files);
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        // 首批全量；其后必须返回 StatusCode::Eof——russh-sftp 客户端只认
        // EOF 状态码收尾，空 Name 会被当作"本批无条目"继续循环（曾致基准挂起）
        match self.dirs.lock().remove(&handle) {
            Some(files) => Ok(Name { id, files }),
            None => Err(StatusCode::Eof),
        }
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

pub struct GateSsh {
    pub root: PathBuf,
    /// 每请求人造延迟（RTT 模拟），new_client 克隆共享
    pub delay_ms: Arc<AtomicU64>,
}

pub struct GateSshHandler {
    root: PathBuf,
    delay_ms: Arc<AtomicU64>,
    channels: tokio::sync::Mutex<HashMap<ChannelId, russh::Channel<Msg>>>,
}

impl SshServer for GateSsh {
    type Handler = GateSshHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> GateSshHandler {
        GateSshHandler {
            root: self.root.clone(),
            delay_ms: self.delay_ms.clone(),
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
            let delay = self.delay_ms.clone();
            session.channel_success(channel_id)?;
            tokio::spawn(async move {
                russh_sftp::server::run(channel.into_stream(), Fs::new(root, delay)).await;
            });
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

/// 返回 (端口, 延迟句柄)——延迟默认 0ms，基准运行时调节
pub async fn start_server(root: PathBuf) -> (u16, Arc<AtomicU64>) {
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
    let delay_ms = Arc::new(AtomicU64::new(0));
    let delay2 = delay_ms.clone();
    tokio::spawn(async move {
        let _ = GateSsh {
            root,
            delay_ms: delay2,
        }
        .run_on_socket(Arc::new(config), &listener)
        .await;
    });
    (port, delay_ms)
}

pub fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("myssh-gate-sftp-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub async fn connect(port: u16) -> SshConnection {
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

// ---- 延迟代理（RTT 模拟的正解：延迟作用在链路上而非服务端处理，
// russh-sftp 服务端逐包 await，handler 内 sleep 会序列化请求，测不出流水线收益）----

/// 每方向固定延迟的 TCP 代理：delay 为单向延迟（RTT/2）。
/// 返回 (代理端口, 延迟句柄)——延迟运行时可调。
pub async fn delay_proxy(target_port: u16, delay_ms: Arc<AtomicU64>) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((inbound, _)) = listener.accept().await {
            let delay = delay_ms.clone();
            tokio::spawn(async move {
                let Ok(outbound) = tokio::net::TcpStream::connect(("127.0.0.1", target_port)).await
                else {
                    return;
                };
                // 两个方向各自一条延迟线：进缓冲 → sleep(单向延迟) → 转发
                // owned split：半句柄 'static 才能进 relay 任务
                let (ir, iw) = inbound.into_split();
                let (or, ow) = outbound.into_split();
                let pipe = async |mut r: tokio::net::tcp::OwnedReadHalf,
                                  mut w: tokio::net::tcp::OwnedWriteHalf,
                                  delay: Arc<AtomicU64>| {
                    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(256);
                    // 真延迟线：每块标交付时刻（now+单向延迟），到点即写——
                    // 块间不互相等待（恒定延迟、不损失带宽）；
                    // 逐块 sleep 的朴素写法会把吞吐压成 chunk_size/delay。
                    let relay = tokio::spawn(async move {
                        let d = delay.clone();
                        let mut queue: std::collections::VecDeque<(tokio::time::Instant, Vec<u8>)> =
                            std::collections::VecDeque::new();
                        loop {
                            tokio::select! {
                                bytes = rx.recv() => {
                                    match bytes {
                                        Some(b) => {
                                            let ms = d.load(Ordering::Relaxed);
                                            queue.push_back((
                                                tokio::time::Instant::now()
                                                    + std::time::Duration::from_millis(ms),
                                                b,
                                            ));
                                        }
                                        None => {
                                            while let Some((at, b)) = queue.pop_front() {
                                                tokio::time::sleep_until(at).await;
                                                if w.write_all(&b).await.is_err() {
                                                    return;
                                                }
                                            }
                                            return;
                                        }
                                    }
                                }
                                _ = async {
                                    let (at, _) = queue.front().unwrap();
                                    tokio::time::sleep_until(*at).await
                                }, if !queue.is_empty() => {
                                    let (_, b) = queue.pop_front().unwrap();
                                    if w.write_all(&b).await.is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    });
                    let mut buf = vec![0u8; 65536];
                    loop {
                        match r.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if tx.send(buf[..n].to_vec()).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    drop(tx); // 排空后让 relay 收尾
                    let _ = relay.await;
                };
                let _ = tokio::join!(pipe(ir, ow, delay.clone()), pipe(or, iw, delay.clone()));
            });
        }
    });
    port
}
