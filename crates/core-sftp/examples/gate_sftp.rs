//! PR-18 性能门禁（PR 级微基准）：TransferQueue 调度 + SFTP 读写回环。
//!
//! 全自包含（服务端在 examples/testkit 共享）：TransferQueue（SFTP_EXEC
//! 执行槽）跑 N 个上传 + N 个下载，输出单任务 median 延迟与聚合吞吐，
//! 超阈值退出码非零。
//!
//!   cargo run --release -p core-sftp --example gate_sftp
//!
//! 阈值口径同 gate_relay：本机首测 median × 约 2 倍裕度。

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "testkit/mod.rs"]
mod testkit;

use std::sync::Arc;
use std::time::Instant;

use testkit::{connect, median, start_server, temp_root};

const TASKS: usize = 40;
const FILE_SIZE: usize = 256 * 1024;
/// 阈值（本机 release 首测 median 134ms/任务、18MiB/s；回归探测取观测值约一半/数倍裕度）
const MEDIAN_TASK_MS_MAX: f64 = 400.0;
const THROUGHPUT_MIBS_MIN: f64 = 9.0;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    let remote_root = temp_root("remote");
    let (port, _delay) = start_server(remote_root.clone()).await;
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
