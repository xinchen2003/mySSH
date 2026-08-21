//! 隧道压测负载发生器。输出单行 JSON 到 stdout（后端捕获进报告）。
//!
//! 模式：
//!   up    —— N 连接全速写（对端是 sink），测隧道上行吞吐
//!   down  —— N 连接全速读（对端是 source），测隧道下行吞吐
//!   churn —— 并发打开 N 连接，各写 16KB 后保持至结束，测并发建连与延迟

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

static TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);
static CONNECT_LAT_US: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static ERRORS: AtomicU64 = AtomicU64::new(0);

#[derive(Parser)]
struct Args {
    #[arg(long)]
    addr: String,
    #[arg(long)]
    mode: String,
    #[arg(long, default_value_t = 10)]
    duration: u64,
    #[arg(long, default_value_t = 8)]
    conns: usize,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let deadline = Duration::from_secs(args.duration);
    let t0 = Instant::now();

    match args.mode.as_str() {
        "up" => run_transfer(&args.addr, args.conns, deadline, Direction::Up).await,
        "down" => run_transfer(&args.addr, args.conns, deadline, Direction::Down).await,
        "churn" => run_churn(&args.addr, args.conns, deadline).await,
        other => anyhow::bail!("unknown mode: {other}"),
    }

    let secs = t0.elapsed().as_secs_f64();
    let bytes = TOTAL_BYTES.load(Ordering::Relaxed);
    let mut lats = CONNECT_LAT_US.lock().unwrap_or_else(|e| e.into_inner()).clone();
    lats.sort_unstable();
    let pct = |p: f64| -> f64 {
        if lats.is_empty() {
            return 0.0;
        }
        let idx = ((p / 100.0 * lats.len() as f64).ceil() as usize)
            .saturating_sub(1)
            .min(lats.len() - 1);
        lats[idx] as f64 / 1000.0
    };

    println!(
        "{}",
        serde_json::json!({
            "mode": args.mode,
            "secs": (secs * 100.0).round() / 100.0,
            "bytes": bytes,
            "mbps": (bytes as f64 / secs / 1e6 * 10.0).round() / 10.0,
            "conns": args.conns,
            "errors": ERRORS.load(Ordering::Relaxed),
            "tcp_connect_ms": { "p50": pct(50.0), "p99": pct(99.0), "max": lats.last().map(|v| *v as f64 / 1000.0).unwrap_or(0.0) },
        })
    );
    Ok(())
}

enum Direction {
    Up,
    Down,
}

async fn run_transfer(addr: &str, conns: usize, deadline: Duration, dir: Direction) {
    let mut tasks = Vec::new();
    for _ in 0..conns {
        let addr = addr.to_string();
        let is_up = matches!(dir, Direction::Up);
        tasks.push(tokio::spawn(async move {
            let t = Instant::now();
            let mut stream = match TcpStream::connect(&addr).await {
                Ok(s) => s,
                Err(_) => {
                    ERRORS.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            record_connect(t.elapsed());
            let _ = stream.set_nodelay(true);
            let end = Instant::now() + deadline;
            if is_up {
                let buf = vec![0x5Au8; 64 * 1024];
                while Instant::now() < end {
                    match stream.write(&buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            TOTAL_BYTES.fetch_add(n as u64, Ordering::Relaxed);
                        }
                    }
                }
            } else {
                let mut buf = vec![0u8; 64 * 1024];
                while Instant::now() < end {
                    match stream.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            TOTAL_BYTES.fetch_add(n as u64, Ordering::Relaxed);
                        }
                    }
                }
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
}

async fn run_churn(addr: &str, conns: usize, deadline: Duration) {
    // 并发建立全部连接，各自写 16KB 后保持，直到 deadline 统一关闭
    let mut tasks = Vec::new();
    for _ in 0..conns {
        let addr = addr.to_string();
        tasks.push(tokio::spawn(async move {
            let t = Instant::now();
            let mut stream = match TcpStream::connect(&addr).await {
                Ok(s) => s,
                Err(_) => {
                    ERRORS.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            };
            record_connect(t.elapsed());
            let payload = vec![0x42u8; 16 * 1024];
            if stream.write_all(&payload).await.is_err() {
                ERRORS.fetch_add(1, Ordering::Relaxed);
                return;
            }
            TOTAL_BYTES.fetch_add(payload.len() as u64, Ordering::Relaxed);
            tokio::time::sleep(deadline).await;
            drop(stream);
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
}

fn record_connect(d: Duration) {
    CONNECT_LAT_US
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(d.as_micros() as u64);
}
