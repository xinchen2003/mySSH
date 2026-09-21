//! PR-16 基准补跑（基准驱动参数定稿）：
//!   1. RTT 矩阵——串行 vs 生产流水线下载（P4-1 量化串行损失）
//!   2. 参数矩阵——depth × chunk × RTT（P4-2；自研裸会话可调参数回路，
//!      与生产 download_pipelined 同算法，只为探参数空间，不改生产常量）
//!   3. list 隔离——传输中 metadata P95 vs 基线（PR-11 量化验收：≤ 2×）
//!
//!   cargo run --release -p core-sftp --example bench_perf
//!
//! 首测结论（本机 release，32MiB）：流水线加速 6.5~7×（RTT 50~200ms，
//! depth=8），RTT 0 时 19.8×；参数矩阵证实吞吐 ≈ depth×chunk/RTT 线性
//! （depth 16 再翻倍，已据此把生产 READ_AHEAD_DEPTH 调为 16）；
//! list 隔离 P95 比值 1.00×（PR-11 验收 PASS）。
//!
//! RTT 由链路延迟代理模拟（testkit delay_proxy，真延迟线：块标交付时刻、
//! 块间不互相等待）。本机回环 + 延迟注入的吞吐绝对值不代表真实网络，
//! 但串行/流水线**比值**与参数排序有效。机器忙时数值上移，比值仍可读。

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "testkit/mod.rs"]
mod testkit;

use std::sync::Arc;
use std::time::{Duration, Instant};

use testkit::{connect, delay_proxy, p95, start_server, temp_root};
use tokio::io::AsyncReadExt;

const FILE_MIB: usize = 32;
const FILE_SIZE: usize = FILE_MIB * 1024 * 1024;
const CHUNK: usize = 256 * 1024;

fn mib_s(bytes: usize, secs: f64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / secs
}

/// 串行参照（= 生产回退路径）：高层 File 单在途读循环
async fn serial_download(client: &core_sftp::SftpClient, remote: &str) -> f64 {
    let mut src = client.open_read(remote).await.unwrap();
    let mut buf = vec![0u8; CHUNK];
    let t0 = Instant::now();
    let mut total = 0usize;
    loop {
        let n = src.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        total += n;
    }
    assert_eq!(total, FILE_SIZE);
    mib_s(total, t0.elapsed().as_secs_f64())
}

/// 生产流水线（TransferQueue → download_pipelined：8 × 256KiB）
async fn prod_pipelined_download(
    q: &Arc<core_sftp::TransferQueue>,
    remote: &str,
    local_root: &std::path::Path,
    tag: &str,
) -> f64 {
    let local = local_root.join(format!("pl-{tag}.bin"));
    let id = q
        .enqueue_download(
            remote.to_string(),
            local.clone(),
            FILE_SIZE as u64,
            core_sftp::OnExists::Overwrite,
        )
        .await;
    let t0 = Instant::now();
    loop {
        let list = q.list();
        let info = list.iter().find(|t| t.id == id);
        if let Some(i) = info {
            if i.state == core_sftp::TransferState::Done {
                break;
            }
            assert!(
                !matches!(
                    i.state,
                    core_sftp::TransferState::Failed | core_sftp::TransferState::Canceled
                ),
                "下载失败: {:?}",
                i.error
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mibs = mib_s(FILE_SIZE, t0.elapsed().as_secs_f64());
    assert_eq!(std::fs::metadata(&local).unwrap().len() as usize, FILE_SIZE);
    let _ = std::fs::remove_file(&local);
    mibs
}

/// 参数矩阵用可调流水线（与生产 download_pipelined 同算法，depth/chunk 可变）
async fn raw_pipelined_download(
    raw: &Arc<russh_sftp::client::RawSftpSession>,
    remote: &str,
    depth: usize,
    chunk: usize,
) -> f64 {
    let handle = raw
        .open(
            remote.to_string(),
            russh_sftp::protocol::OpenFlags::READ,
            russh_sftp::protocol::FileAttributes::default(),
        )
        .await
        .unwrap()
        .handle;
    let t0 = Instant::now();
    let total = FILE_SIZE as u64;
    let mut next = 0u64;
    let mut got = 0u64;
    let mut in_flight: tokio::task::JoinSet<Vec<u8>> = tokio::task::JoinSet::new();
    while next < total || !in_flight.is_empty() {
        while in_flight.len() < depth && next < total {
            let len = (total - next).min(chunk as u64);
            let at = next;
            next += len;
            let raw2 = raw.clone();
            let h = handle.clone();
            in_flight.spawn(async move { raw2.read(h, at, len as u32).await.unwrap().data });
        }
        if let Some(Ok(data)) = in_flight.join_next().await {
            got += data.len() as u64;
        }
    }
    assert_eq!(got, total);
    let mibs = mib_s(FILE_SIZE, t0.elapsed().as_secs_f64());
    let _ = raw.close(handle).await;
    mibs
}

/// 裸会话（参数矩阵用；复刻 SftpClient::open_download_raw 握手）
async fn open_raw(conn: &core_ssh::SshConnection) -> Arc<russh_sftp::client::RawSftpSession> {
    use russh_sftp::client::{Config, RawSftpSession};
    let ch = conn.open_session_channel().await.unwrap();
    ch.request_subsystem(true, "sftp").await.unwrap();
    let mut raw = RawSftpSession::new_with_config(ch.into_stream(), Config::default());
    let version = raw.init().await.unwrap();
    if version
        .extensions
        .get(russh_sftp::extensions::LIMITS)
        .is_some_and(|v| v == "1")
    {
        let limits = russh_sftp::client::rawsession::Limits::from(raw.limits().await.unwrap());
        raw.set_limits(limits);
    }
    Arc::new(raw)
}

async fn list_p95(client: &core_sftp::SftpClient, dir: &str, n: usize) -> f64 {
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let t0 = Instant::now();
        let entries = client.list(dir).await.unwrap();
        assert_eq!(entries.len(), 200);
        v.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    p95(v)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let remote_root = temp_root("bench-remote");
    let local_root = temp_root("bench-local");
    std::fs::write(remote_root.join("big.bin"), vec![0xabu8; FILE_SIZE]).unwrap();
    // list 隔离验收目录（200 项）
    let listdir = remote_root.join("listdir");
    std::fs::create_dir_all(&listdir).unwrap();
    for i in 0..200 {
        std::fs::write(listdir.join(format!("f{i}.bin")), b"x").unwrap();
    }
    let (server_port, _handler_delay) = start_server(remote_root).await;
    // 延迟代理：RTT 作用在链路上（russh-sftp 服务端逐包 await，handler 内
    // sleep 会序列化请求，测不出流水线收益）；单向延迟 = RTT/2
    let link_delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let port = delay_proxy(server_port, link_delay.clone()).await;

    // ============ 1. RTT 矩阵：串行 vs 生产流水线 ============
    println!("\n== RTT 矩阵（32MiB 下载；串行 vs 生产流水线 8×256KiB）==");
    println!(
        "{:>8} {:>12} {:>12} {:>8}",
        "RTT_ms", "serial_MiB", "pipelined", "加速比"
    );
    // 档位对齐计划（1/50/100/200）；1ms 低于 Windows 计时粒度不可测，取 0 为近端参照
    for rtt in [0u64, 50, 100, 200] {
        link_delay.store(rtt / 2, std::sync::atomic::Ordering::Relaxed);
        // 串行用独立连接（不占队列执行槽；读循环纯客户端行为）
        let conn = connect(port).await;
        let client = core_sftp::SftpClient::open(&conn).await.unwrap();
        let serial = serial_download(&client, "/big.bin").await;
        drop(conn);

        let conn = connect(port).await;
        let slot = core_sftp::SftpSlot::open_sftp(
            "bench",
            Arc::new(conn),
            tokio::runtime::Handle::current(),
        )
        .await
        .unwrap();
        let q = Arc::new(core_sftp::TransferQueue::new(
            slot,
            core_policy::budget::caps::SFTP_EXEC,
            tokio::runtime::Handle::current(),
        ));
        let pipelined =
            prod_pipelined_download(&q, "/big.bin", &local_root, &format!("rtt{rtt}")).await;
        println!(
            "{rtt:>8} {serial:>12.1} {pipelined:>12.1} {:>7.1}x",
            pipelined / serial
        );
    }
    link_delay.store(0, std::sync::atomic::Ordering::Relaxed);

    // ============ 2. 参数矩阵：depth × chunk × RTT ============
    let conn = connect(port).await;
    let raw = open_raw(&conn).await;
    println!("\n== 参数矩阵（32MiB；depth × chunk × RTT，MiB/s）==");
    println!(
        "{:>8} {:>8} {:>8} {:>10}",
        "RTT_ms", "depth", "chunk_K", "MiB_s"
    );
    for rtt in [50u64, 200] {
        link_delay.store(rtt / 2, std::sync::atomic::Ordering::Relaxed);
        for depth in [4usize, 8, 16] {
            for chunk_k in [64usize, 256] {
                let mibs = raw_pipelined_download(&raw, "/big.bin", depth, chunk_k * 1024).await;
                println!("{rtt:>8} {depth:>8} {chunk_k:>8} {mibs:>10.1}");
            }
        }
    }
    link_delay.store(0, std::sync::atomic::Ordering::Relaxed);

    // ============ 3. list 隔离（PR-11 验收：传输中 P95 ≤ 2× 基线）============
    println!("\n== list 隔离（200 项目录，50 次采样，P95 ms）==");
    let conn = Arc::new(connect(port).await);
    let slot =
        core_sftp::SftpSlot::open_sftp("bench", conn.clone(), tokio::runtime::Handle::current())
            .await
            .unwrap();
    // PR-11 拓扑：同一 SshConnection 上两条 SFTP subsystem（meta/data 双 slot）
    let meta_slot = core_sftp::SftpSlot::open_sftp(
        "bench-meta",
        conn.clone(),
        tokio::runtime::Handle::current(),
    )
    .await
    .unwrap();
    // 基线：无传输时 metadata 通道 P95
    let meta_client = meta_slot.get().await.unwrap();
    let baseline = list_p95(&meta_client, "/listdir", 50).await;
    // 传输中：data slot 跑流水线大下载，metadata 通道同时 list
    let q = Arc::new(core_sftp::TransferQueue::new(
        slot.clone(),
        core_policy::budget::caps::SFTP_EXEC,
        tokio::runtime::Handle::current(),
    ));
    let dl = {
        let q = q.clone();
        let local_root = local_root.clone();
        tokio::spawn(async move {
            prod_pipelined_download(&q, "/big.bin", &local_root, "isolate").await
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await; // 等下载进入稳态
    let during = list_p95(&meta_client, "/listdir", 50).await;
    dl.await.unwrap();
    let ratio = during / baseline;
    println!("baseline_p95={baseline:.1}ms  during_transfer_p95={during:.1}ms  ratio={ratio:.2}x");
    println!(
        "PR-11 验收（≤2x）: {}",
        if ratio <= 2.0 { "PASS" } else { "FAIL" }
    );
}
