//! core-sftp 集成测试：内存 russh 服务端 + russh-sftp FS 后端（tempdir 根），
//! 验证 SftpClient 的目录/元数据/递归删除/重命名/权限链路。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use core_ssh::{
    AuthMethod, ConnClass, ConnectOptions, HostKeyCheck, KeepaliveConfig, SshConnection,
};
use russh::keys::{Algorithm, PrivateKey};
use russh::server::{Auth, ChannelOpenHandle, Handler as SshHandler, Msg, Server, Session};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use tokio::net::TcpListener;

// ---------- FS 后端 SFTP 服务端（tempdir 根；供全部 core-sftp 测试复用） ----------

pub struct FsSftp {
    root: PathBuf,
    files: Mutex<HashMap<String, std::fs::File>>,
    /// 目录句柄 → (路径, 是否已返回过条目)；SFTP readdir 协议需多次调用直到 EOF
    dirs: Mutex<HashMap<String, (String, bool)>>,
    /// SLOW 路径对应的文件句柄（写路径限速门控）
    slow_handles: Mutex<std::collections::HashSet<String>>,
    next_handle: AtomicU64,
}

impl FsSftp {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            files: Mutex::new(HashMap::new()),
            dirs: Mutex::new(HashMap::new()),
            slow_handles: Mutex::new(std::collections::HashSet::new()),
            next_handle: AtomicU64::new(1),
        }
    }

    /// 防目录逃逸：只允许 root 内的相对路径
    fn resolve(&self, p: &str) -> Result<PathBuf, StatusCode> {
        let rel = p.trim_start_matches('/');
        let joined = self.root.join(rel);
        if rel.contains("..") {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(joined)
    }

    fn attrs_of(&self, meta: &std::fs::Metadata) -> FileAttributes {
        use std::os::windows::fs::MetadataExt;
        let size = meta.len();
        // Windows 无 POSIX mode；目录/只读可判定，其余给常规默认
        let mode = if meta.is_dir() {
            0o040000 | 0o755
        } else if meta.is_symlink() {
            0o120000 | 0o777
        } else {
            0o100000 | 0o644
        };
        let mtime = meta
            .last_write_time()
            .checked_div(1_000_000_000)
            .map(|v| v as u32);
        FileAttributes {
            size: Some(size),
            permissions: Some(mode),
            mtime,
            ..Default::default()
        }
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

impl russh_sftp::server::Handler for FsSftp {
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
        let slow = filename.contains("SLOW");
        let path = self.resolve(&filename)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        if pflags.contains(OpenFlags::WRITE) {
            opts.write(true);
        }
        if pflags.contains(OpenFlags::CREATE) {
            opts.create(true);
        }
        if pflags.contains(OpenFlags::TRUNCATE) {
            opts.truncate(true);
        }
        if pflags.contains(OpenFlags::APPEND) {
            opts.append(true);
        }
        let f = opts.open(&path).map_err(|_| StatusCode::NoSuchFile)?;
        let h = format!("h{}", self.next_handle.fetch_add(1, Ordering::Relaxed));
        self.files.lock().unwrap().insert(h.clone(), f);
        if slow {
            self.slow_handles.lock().unwrap().insert(h.clone());
        }
        Ok(Handle { id, handle: h })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        self.files.lock().unwrap().remove(&handle);
        self.dirs.lock().unwrap().remove(&handle);
        self.slow_handles.lock().unwrap().remove(&handle);
        Ok(Self::ok(id))
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let mut files = self.files.lock().unwrap();
        let f = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        f.seek(SeekFrom::Start(offset))
            .map_err(|_| StatusCode::Failure)?;
        let mut buf = vec![0u8; len as usize];
        let n = f.read(&mut buf).map_err(|_| StatusCode::Failure)?;
        if n == 0 {
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        // 门控：SLOW 路径每写块睡 50ms（给暂停/取消留出确定性窗口）
        let slow = self.slow_handles.lock().unwrap().contains(&handle);
        if slow {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let mut files = self.files.lock().unwrap();
        let f = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        f.seek(SeekFrom::Start(offset))
            .map_err(|_| StatusCode::Failure)?;
        f.write_all(&data).map_err(|_| StatusCode::Failure)?;
        Ok(Self::ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let p = self.resolve(&path)?;
        let meta = std::fs::symlink_metadata(&p).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: self.attrs_of(&meta),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let p = self.resolve(&path)?;
        let meta = std::fs::metadata(&p).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(Attrs {
            id,
            attrs: self.attrs_of(&meta),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        // Windows FS 无 chmod 语义；接受即成功（测试只验证协议链路）
        self.resolve(&path)?;
        Ok(Self::ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        _handle: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        Ok(Self::ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        let p = self.resolve(&path)?;
        if !p.is_dir() {
            return Err(StatusCode::NoSuchFile);
        }
        let h = format!("d{}", self.next_handle.fetch_add(1, Ordering::Relaxed));
        self.dirs.lock().unwrap().insert(h.clone(), (path, false));
        Ok(Handle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        let path = {
            let mut dirs = self.dirs.lock().unwrap();
            let (path, done) = dirs.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
            if *done {
                return Err(StatusCode::Eof);
            }
            *done = true;
            path.clone()
        };
        let p = self.resolve(&path)?;
        let mut files = Vec::new();
        for e in std::fs::read_dir(&p).map_err(|_| StatusCode::Failure)? {
            let e = e.map_err(|_| StatusCode::Failure)?;
            let meta = e.metadata().map_err(|_| StatusCode::Failure)?;
            let name = e.file_name().to_string_lossy().to_string();
            files.push(File::new(name, self.attrs_of(&meta)));
        }
        if files.is_empty() {
            return Err(StatusCode::Eof);
        }
        Ok(Name { id, files })
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        let p = self.resolve(&filename)?;
        std::fs::remove_file(&p).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(Self::ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let p = self.resolve(&path)?;
        std::fs::create_dir(&p).map_err(|_| StatusCode::Failure)?;
        Ok(Self::ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        let p = self.resolve(&path)?;
        std::fs::remove_dir(&p).map_err(|_| StatusCode::Failure)?;
        Ok(Self::ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        Ok(Name {
            id,
            files: vec![File::new(path, FileAttributes::default())],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        std::fs::rename(self.resolve(&oldpath)?, self.resolve(&newpath)?)
            .map_err(|_| StatusCode::Failure)?;
        Ok(Self::ok(id))
    }
}

// ---------- SSH 服务端壳 ----------

struct TestServer {
    root: PathBuf,
}
struct TestHandler {
    root: PathBuf,
    channels: tokio::sync::Mutex<HashMap<ChannelId, Channel<Msg>>>,
}

impl Server for TestServer {
    type Handler = TestHandler;
    fn new_client(&mut self, _addr: Option<std::net::SocketAddr>) -> TestHandler {
        TestHandler {
            root: self.root.clone(),
            channels: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl SshHandler for TestHandler {
    type Error = russh::Error;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
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
            let channel = self.channels.lock().await.remove(&channel_id).unwrap();
            let root = self.root.clone();
            session.channel_success(channel_id)?;
            tokio::spawn(async move {
                russh_sftp::server::run(channel.into_stream(), FsSftp::new(root)).await;
            });
        } else {
            session.channel_failure(channel_id)?;
        }
        Ok(())
    }
}

pub async fn start_sftp_server(root: PathBuf) -> u16 {
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
        let mut server = TestServer { root };
        let _ = server.run_on_socket(Arc::new(config), &listener).await;
    });
    port
}

pub async fn connect(port: u16) -> SshConnection {
    SshConnection::connect(ConnectOptions {
        host: "127.0.0.1".into(),
        port,
        user: "test".into(),
        auth: AuthMethod::None,
        class: ConnClass::Bulk,
        window_size: 4 * 1024 * 1024,
        max_packet_size: 32768,
        keepalive: KeepaliveConfig::default(),
        jump_chain: vec![],
        host_key_check: HostKeyCheck::AcceptAll,
        ki_prompter: None,
    })
    .await
    .expect("connect")
}

pub fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("myssh-sftp-test-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------- 测试 ----------

#[tokio::test]
async fn list_stat_mkdir_rename_remove() {
    let root = temp_root("ops");
    std::fs::create_dir_all(root.join("sub/inner")).unwrap();
    std::fs::write(root.join("a.txt"), b"hello").unwrap();
    std::fs::write(root.join("sub/b.bin"), vec![0u8; 1024]).unwrap();

    let port = start_sftp_server(root.clone()).await;
    let conn = connect(port).await;
    let sftp = core_sftp::SftpClient::open(&conn).await.expect("sftp open");

    // list 根目录：目录在前
    let entries = sftp.list("/").await.expect("list /");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "sub");
    assert_eq!(entries[0].kind, core_sftp::EntryKind::Dir);
    assert_eq!(entries[1].name, "a.txt");
    assert_eq!(entries[1].size, 5);

    // stat
    let st = sftp.stat("/a.txt").await.expect("stat");
    assert_eq!(st.size, 5);
    assert_eq!(st.kind, core_sftp::EntryKind::File);

    // mkdir + rename
    sftp.mkdir("/newdir").await.expect("mkdir");
    assert!(root.join("newdir").is_dir());
    sftp.rename("/a.txt", "/renamed.txt").await.expect("rename");
    assert!(!root.join("a.txt").exists() && root.join("renamed.txt").exists());

    // chmod（协议链路；Windows FS 不验证位）
    sftp.chmod("/renamed.txt", 0o600).await.expect("chmod");

    // 递归删除
    sftp.remove_recursive("/sub")
        .await
        .expect("remove_recursive");
    assert!(!root.join("sub").exists());
}

#[tokio::test]
async fn read_write_roundtrip() {
    let root = temp_root("rw");
    let port = start_sftp_server(root.clone()).await;
    let conn = connect(port).await;
    let sftp = core_sftp::SftpClient::open(&conn).await.expect("sftp open");

    // 经 open_write_at/open_read 走一遍（传输队列的原语）
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let payload: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let mut w = sftp.open_write_at("/big.bin", 0).await.expect("open write");
    w.write_all(&payload).await.expect("write");
    w.shutdown().await.expect("shutdown");
    drop(w);

    assert_eq!(
        std::fs::metadata(root.join("big.bin")).unwrap().len(),
        200_000
    );

    let mut r = sftp.open_read("/big.bin").await.expect("open read");
    let mut got = Vec::new();
    r.read_to_end(&mut got).await.expect("read");
    assert_eq!(got, payload);

    // 续传语义：offset 定位写（shutdown 排空写确认，避免与后续读竞态）
    let mut w2 = sftp
        .open_write_at("/big.bin", 100)
        .await
        .expect("open at 100");
    w2.write_all(b"XYZ").await.expect("write at");
    w2.shutdown().await.expect("flush");
    drop(w2);
    let mut expect = payload.clone();
    expect[100..103].copy_from_slice(b"XYZ");
    let got2 = std::fs::read(root.join("big.bin")).unwrap();
    assert_eq!(got2, expect);
}

#[tokio::test]
async fn lstat_and_error_paths() {
    let root = temp_root("err");
    let port = start_sftp_server(root).await;
    let conn = connect(port).await;
    let sftp = core_sftp::SftpClient::open(&conn).await.expect("sftp open");

    // 不存在路径 → RemotePath 错误（E5002）
    let err = sftp.stat("/nope").await.expect_err("should fail");
    assert!(err.to_string().starts_with("E5002"), "{err}");
}

// ---------- TransferQueue 测试 ----------

fn pattern(len: usize) -> Vec<u8> {
    (0..len as u32).map(|i| (i % 251) as u8).collect()
}

/// 等传输到达终态（最多 10s）
async fn wait_done(q: &core_sftp::TransferQueue, id: &str) -> core_sftp::TransferInfo {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let info = q.get(id).expect("transfer exists");
        match info.state {
            core_sftp::TransferState::Done
            | core_sftp::TransferState::Failed
            | core_sftp::TransferState::Canceled => return info,
            _ => {}
        }
        assert!(
            std::time::Instant::now() < deadline,
            "transfer not settled: {info:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn transfer_upload_download_integrity() {
    let root = temp_root("xfer");
    let port = start_sftp_server(root.clone()).await;
    let conn = connect(port).await;
    let sftp = std::sync::Arc::new(core_sftp::SftpClient::open(&conn).await.expect("sftp"));
    let q = std::sync::Arc::new(core_sftp::TransferQueue::new(
        sftp,
        3,
        tokio::runtime::Handle::current(),
    ));

    let payload = pattern(1024 * 1024 + 7); // 1MB+ 跨块
    let local_up = temp_root("xfer-local").join("up.bin");
    std::fs::write(&local_up, &payload).unwrap();

    let id = q
        .enqueue_upload(local_up.clone(), "/up.bin".into(), payload.len() as u64)
        .await;
    let info = wait_done(&q, &id).await;
    assert_eq!(
        info.state,
        core_sftp::TransferState::Done,
        "{:?}",
        info.error
    );
    assert_eq!(info.bytes_done, payload.len() as u64);
    assert_eq!(std::fs::read(root.join("up.bin")).unwrap(), payload);

    // 下载回本地另一路径
    let local_down = temp_root("xfer-local").join("down.bin");
    let id2 = q
        .enqueue_download("/up.bin".into(), local_down.clone(), payload.len() as u64)
        .await;
    let info2 = wait_done(&q, &id2).await;
    assert_eq!(
        info2.state,
        core_sftp::TransferState::Done,
        "{:?}",
        info2.error
    );
    assert_eq!(std::fs::read(&local_down).unwrap(), payload);
}

#[tokio::test]
async fn download_resumes_from_local_offset() {
    let root = temp_root("resume");
    std::fs::write(root.join("full.bin"), pattern(200_000)).unwrap();
    let port = start_sftp_server(root).await;
    let conn = connect(port).await;
    let sftp = std::sync::Arc::new(core_sftp::SftpClient::open(&conn).await.expect("sftp"));
    let q = std::sync::Arc::new(core_sftp::TransferQueue::new(
        sftp,
        2,
        tokio::runtime::Handle::current(),
    ));

    // 预置 50KB 半成品（断点）
    let local = temp_root("resume-local").join("full.bin");
    std::fs::write(&local, &pattern(200_000)[..50_000]).unwrap();

    let id = q
        .enqueue_download("/full.bin".into(), local.clone(), 200_000)
        .await;
    let info = wait_done(&q, &id).await;
    assert_eq!(
        info.state,
        core_sftp::TransferState::Done,
        "{:?}",
        info.error
    );
    assert_eq!(std::fs::read(&local).unwrap(), pattern(200_000));
}

#[tokio::test]
async fn pause_then_cancel_slow_upload() {
    let root = temp_root("slow");
    let port = start_sftp_server(root.clone()).await;
    let conn = connect(port).await;
    let sftp = std::sync::Arc::new(core_sftp::SftpClient::open(&conn).await.expect("sftp"));
    let q = std::sync::Arc::new(core_sftp::TransferQueue::new(
        sftp,
        2,
        tokio::runtime::Handle::current(),
    ));

    // 进度回调应被触发
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let hits2 = hits.clone();
    q.set_progress_callback(std::sync::Arc::new(move |_| {
        hits2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }));

    let local = temp_root("slow-local").join("big.SLOW.bin");
    std::fs::write(&local, pattern(4 * 1024 * 1024)).unwrap(); // 16 块 × 50ms ≈ 800ms 窗口
    let id = q
        .enqueue_upload(local, "/big.SLOW.bin".into(), 4 * 1024 * 1024)
        .await;

    // 等开跑后暂停
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let info = q.get(&id).unwrap();
        if info.state == core_sftp::TransferState::Running && info.bytes_done > 0 {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "never started");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    q.pause(&id).unwrap();
    let paused = wait_state(&q, &id, core_sftp::TransferState::Paused).await;
    let frozen = paused.bytes_done;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(q.get(&id).unwrap().bytes_done, frozen, "暂停后字节数应冻结");

    // 取消 → Canceled，远端只落了部分
    q.cancel(&id).unwrap();
    let final_info = wait_done(&q, &id).await;
    assert_eq!(final_info.state, core_sftp::TransferState::Canceled);
    assert!(final_info.bytes_done < 4 * 1024 * 1024);
    assert!(
        hits.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "进度回调未触发"
    );
}

async fn wait_state(
    q: &core_sftp::TransferQueue,
    id: &str,
    want: core_sftp::TransferState,
) -> core_sftp::TransferInfo {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let info = q.get(id).expect("transfer exists");
        if info.state == want {
            return info;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "never reached {want:?}: {info:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
