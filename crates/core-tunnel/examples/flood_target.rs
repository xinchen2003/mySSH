//! 吞吐测试目标/客户端（隧道 ≥400MB/s 预算复测，docs/design/10-risks.md）。
//!
//!   cargo run -p core-tunnel --example flood_target -- server 127.0.0.1:9998
//!   cargo run -p core-tunnel --example flood_target -- client 127.0.0.1:18081 5
//!
//! server：每连接 256KB 块循环写。client：读 N 秒报均值速率。

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("server") => {
            let addr = args.get(2).map(String::as_str).unwrap_or("127.0.0.1:9998");
            let listener = TcpListener::bind(addr)
                .await
                .unwrap_or_else(|e| panic!("bind: {e}"));
            println!("flood server on {addr}");
            loop {
                let (mut s, _) = listener
                    .accept()
                    .await
                    .unwrap_or_else(|e| panic!("accept: {e}"));
                tokio::spawn(async move {
                    let block = vec![0xABu8; 256 * 1024];
                    loop {
                        if s.write_all(&block).await.is_err() {
                            break;
                        }
                    }
                });
            }
        }
        Some("client") => {
            let addr = args.get(2).map(String::as_str).unwrap_or("127.0.0.1:18081");
            let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(5);
            let mut s = TcpStream::connect(addr)
                .await
                .unwrap_or_else(|e| panic!("connect: {e}"));
            let mut buf = vec![0u8; 256 * 1024];
            let t0 = std::time::Instant::now();
            let mut total = 0u64;
            while t0.elapsed().as_secs() < secs {
                match s.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => total += n as u64,
                    Err(_) => break,
                }
            }
            let secs_f = t0.elapsed().as_secs_f64();
            println!(
                "total {:.1} MB in {:.2}s = {:.1} MB/s",
                total as f64 / 1048576.0,
                secs_f,
                total as f64 / 1048576.0 / secs_f
            );
        }
        _ => eprintln!("usage: flood_target server [addr] | client [addr] [secs]"),
    }
}
