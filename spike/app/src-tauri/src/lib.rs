//! Spike 应用后端：终端数据通路（聚合+背压）与隧道负载编排。

mod ssh;
mod stats;
mod tunnel;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh::client::Msg;
use russh::{ChannelMsg, ChannelWriteHalf};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, Response};
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

/// 输出聚合时间窗（规格书第 2 条）
const AGG_WINDOW: Duration = Duration::from_millis(8);
/// 单次推送上限（规格书第 2 条）
const AGG_CAP: usize = 256 * 1024;
/// 前端未确认字节数上限（信用背压；超出即停止从 SSH 读取，窗口自然耗尽）
const CREDIT_HIGH: usize = 8 * 1024 * 1024;

struct AppState {
    client: tokio::sync::Mutex<Option<ssh::ClientHandle>>,
    echo_write: tokio::sync::Mutex<Option<ChannelWriteHalf<Msg>>>,
    /// 前端信用：初始 CREDIT_HIGH 个许可，每推送 N 字节取 N 个，credit 命令归还
    credits: Arc<Semaphore>,
    /// 已推送未确认字节数（可观测性）
    outstanding: Arc<AtomicI64>,
    tunnels: tunnel::TunnelHandle,
}

/// 后端阶段事件（name, boundary, tWall）
static PHASE_EVENTS: Mutex<Vec<(String, String, u64)>> = Mutex::new(Vec::new());
/// loadgen 各阶段 stdout JSON
static LOADGEN_RESULTS: Mutex<Vec<Value>> = Mutex::new(Vec::new());
/// 阶段边界隧道计数器快照
static TUNNEL_SNAPS: Mutex<Vec<(String, stats::TunnelSnapshot)>> = Mutex::new(Vec::new());

fn record_phase(name: &str, boundary: &str) -> u64 {
    let t = stats::now_wall_ms();
    PHASE_EVENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((name.to_string(), boundary.to_string(), t));
    TUNNEL_SNAPS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((format!("{name}.{boundary}"), stats::TunnelSnapshot::now()));
    t
}

fn emit_phase(stats_ch: &Channel<Value>, name: &str, boundary: &str) {
    let t = record_phase(name, boundary);
    let _ = stats_ch.send(json!({
        "type": "phase",
        "name": name,
        "boundary": boundary,
        "tWall": t,
    }));
}

// ---------- 命令 ----------

#[tauri::command]
async fn connect(state: tauri::State<'_, AppState>) -> Result<(), String> {
    let handle = ssh::connect().await.map_err(|e| e.to_string())?;
    *state.client.lock().await = Some(handle);
    Ok(())
}

#[tauri::command]
async fn open_echo(data: Channel<Response>, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let client = state.client.lock().await.clone().ok_or("not connected")?;
    let channel = client
        .channel_open_session()
        .await
        .map_err(|e| e.to_string())?;
    let (mut read, write) = channel.split();
    write.exec(false, "echo").await.map_err(|e| e.to_string())?;
    *state.echo_write.lock().await = Some(write);

    // echo 回显直推前端（不聚合 —— 探针路径求真实延迟）
    tauri::async_runtime::spawn(async move {
        loop {
            match read.wait().await {
                Some(ChannelMsg::Data { data: payload }) => {
                    if data.send(Response::new(payload.to_vec())).is_err() {
                        break;
                    }
                }
                Some(ChannelMsg::Close) | Some(ChannelMsg::Eof) | None => break,
                _ => {}
            }
        }
    });
    Ok(())
}

#[tauri::command]
async fn send_input(bytes: Vec<u8>, state: tauri::State<'_, AppState>) -> Result<(), String> {
    let guard = state.echo_write.lock().await;
    let w = guard.as_ref().ok_or("echo not open")?;
    // 输入方向零延迟直发，不聚合（规格书第 2 条）
    w.data_bytes(bytes::Bytes::from(bytes))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn credit(bytes: u64, state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.outstanding.fetch_sub(bytes as i64, Ordering::Relaxed);
    state.credits.add_permits(bytes as usize);
    Ok(())
}

#[tauri::command]
async fn start_stream(
    data: Channel<Response>,
    stats: Channel<Value>,
    size_mb: u64,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    let client = state.client.lock().await.clone().ok_or("not connected")?;
    let credits = state.credits.clone();
    let outstanding = state.outstanding.clone();

    tauri::async_runtime::spawn(async move {
        let t0 = Instant::now();
        match stream_loop(client, size_mb, &data, &credits, &outstanding).await {
            Ok(bytes) => {
                let _ = stats.send(json!({
                    "type": "stream_done",
                    "bytes": bytes,
                    "secs": t0.elapsed().as_secs_f64(),
                    "tWall": stats::now_wall_ms(),
                }));
            }
            Err(e) => {
                error!(?e, "stream failed");
                let _ = stats.send(json!({ "type": "log", "msg": format!("stream error: {e}") }));
            }
        }
    });
    Ok(())
}

/// 终端读取循环：8ms/256KB 聚合 + 信用背压。
/// 信用耗尽即停止 ch.wait() → russh 不再确认窗口 → 服务端停发，内存不堆积。
async fn stream_loop(
    client: ssh::ClientHandle,
    size_mb: u64,
    data_ch: &Channel<Response>,
    credits: &Semaphore,
    outstanding: &AtomicI64,
) -> anyhow::Result<u64> {
    let mut ch = client.channel_open_session().await?;
    ch.exec(false, format!("stream {}", size_mb * 1024 * 1024))
        .await?;

    let mut total = 0u64;
    let mut agg: Vec<u8> = Vec::with_capacity(AGG_CAP);
    let mut flush_at = Instant::now() + AGG_WINDOW;

    loop {
        let delay = tokio::time::sleep_until(tokio::time::Instant::from_std(flush_at));
        tokio::pin!(delay);
        tokio::select! {
            msg = ch.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        total += data.len() as u64;
                        agg.extend_from_slice(&data);
                        if agg.len() >= AGG_CAP {
                            flush(data_ch, &mut agg, credits, outstanding).await;
                            flush_at = Instant::now() + AGG_WINDOW;
                        }
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        flush(data_ch, &mut agg, credits, outstanding).await;
                        break;
                    }
                    _ => {}
                }
            }
            _ = &mut delay => {
                if !agg.is_empty() {
                    flush(data_ch, &mut agg, credits, outstanding).await;
                }
                flush_at = Instant::now() + AGG_WINDOW;
            }
        }
    }
    Ok(total)
}

async fn flush(
    data_ch: &Channel<Response>,
    agg: &mut Vec<u8>,
    credits: &Semaphore,
    outstanding: &AtomicI64,
) {
    if agg.is_empty() {
        return;
    }
    let buf = std::mem::replace(agg, Vec::with_capacity(AGG_CAP));
    // 等待前端信用 —— 背压点；等待期间读取循环挂起
    if credits.acquire_many(buf.len() as u32).await.is_err() {
        return;
    }
    outstanding.fetch_add(buf.len() as i64, Ordering::Relaxed);
    let _ = data_ch.send(Response::new(buf));
}

#[tauri::command]
async fn start_tunnel(
    listen_port: u16,
    target_port: u16,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    state
        .tunnels
        .start(listen_port, target_port)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stop_tunnels(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.tunnels.stop_all();
    Ok(())
}

fn loadgen_path() -> PathBuf {
    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join(profile)
        .join("spike-loadgen.exe")
}

async fn run_loadgen_phase(
    stats_ch: &Channel<Value>,
    name: &str,
    addr: &str,
    mode: &str,
    duration: u64,
    conns: usize,
) {
    emit_phase(stats_ch, name, "start");
    let exe = loadgen_path();
    let out = tokio::process::Command::new(&exe)
        .args([
            "--addr", addr,
            "--mode", mode,
            "--duration", &duration.to_string(),
            "--conns", &conns.to_string(),
        ])
        .output()
        .await;
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let parsed: Value =
                serde_json::from_str(stdout.trim()).unwrap_or_else(|_| json!({ "raw": stdout }));
            LOADGEN_RESULTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(json!({ "phase": name, "result": parsed }));
        }
        Err(e) => {
            warn!(?e, ?exe, "loadgen spawn failed");
            LOADGEN_RESULTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(json!({ "phase": name, "error": e.to_string() }));
        }
    }
    emit_phase(stats_ch, name, "end");
}

#[tauri::command]
async fn run_load_sequence(stats: Channel<Value>) -> Result<(), String> {
    tauri::async_runtime::spawn(async move {
        // 上行压测：loadgen → 隧道 → sink
        run_loadgen_phase(&stats, "load_up", "127.0.0.1:13389", "up", 12, 8).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        // 下行压测：source → 隧道 → loadgen
        run_loadgen_phase(&stats, "load_down", "127.0.0.1:13390", "down", 12, 8).await;
        tokio::time::sleep(Duration::from_secs(3)).await;
        // 高并发建连：500 并发短连接
        run_loadgen_phase(&stats, "churn", "127.0.0.1:13389", "churn", 10, 500).await;
        let _ = stats.send(json!({ "type": "all_done" }));
    });
    Ok(())
}

// ---------- 报告合并与判定 ----------

#[derive(Deserialize)]
struct PhaseRange {
    start: Option<u64>,
    end: Option<u64>,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct RttSample {
    t: u64,
    rtt: f64,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct FpsBucket {
    t: u64,
    fps: u64,
}

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct FrontendReport {
    renderer: String,
    phases: HashMap<String, PhaseRange>,
    rtts: Vec<RttSample>,
    #[serde(rename = "fpsBuckets")]
    fps_buckets: Vec<FpsBucket>,
    #[serde(rename = "streamBytes")]
    stream_bytes: u64,
    #[serde(rename = "maxQueueBytes")]
    max_queue_bytes: u64,
    #[serde(rename = "cbLatMs")]
    cb_lat_ms: Vec<f64>,
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((p / 100.0 * sorted.len() as f64).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[idx]
}

fn lat_stats(samples: &[f64]) -> Value {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    json!({
        "count": s.len(),
        "p50": percentile(&s, 50.0),
        "p99": percentile(&s, 99.0),
        "max": s.last().copied().unwrap_or(0.0),
    })
}

#[tauri::command]
async fn finalize_report(frontend: Value) -> Result<Value, String> {
    let fe: FrontendReport =
        serde_json::from_value(frontend.clone()).map_err(|e| e.to_string())?;

    // 合并阶段边界：前端（baseline/stream/idle2）+ 后端（load_*/churn）
    let mut ranges: HashMap<String, (Option<u64>, Option<u64>)> = HashMap::new();
    for (name, r) in &fe.phases {
        ranges.insert(name.clone(), (r.start, r.end));
    }
    for (name, boundary, t) in PHASE_EVENTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
    {
        let entry = ranges.entry(name.clone()).or_default();
        if boundary == "start" {
            entry.0 = Some(*t);
        } else {
            entry.1 = Some(*t);
        }
    }

    // 分阶段按键延迟
    let mut latency_by_phase = serde_json::Map::new();
    for (name, (start, end)) in &ranges {
        let (Some(s), Some(e)) = (start, end) else { continue };
        let samples: Vec<f64> = fe
            .rtts
            .iter()
            .filter(|r| r.t >= *s && r.t < *e)
            .map(|r| r.rtt)
            .collect();
        latency_by_phase.insert(name.clone(), lat_stats(&samples));
    }

    // fps：流阶段与隧道负载阶段
    let fps_in = |start: Option<u64>, end: Option<u64>| -> Vec<u64> {
        let (Some(s), Some(e)) = (start, end) else { return vec![] };
        fe.fps_buckets
            .iter()
            .filter(|b| b.t >= s && b.t < e)
            .map(|b| b.fps)
            .collect()
    };
    let fps_summary = |xs: &[u64]| -> Value {
        if xs.is_empty() {
            return json!({ "avg": 0, "min": 0 });
        }
        json!({
            "avg": xs.iter().sum::<u64>() as f64 / xs.len() as f64,
            "min": xs.iter().min().copied().unwrap_or(0),
        })
    };
    let stream_range = ranges.get("stream").copied().unwrap_or_default();
    let load_all: Vec<u64> = ["load_up", "load_down", "churn"]
        .iter()
        .flat_map(|p| fps_in(ranges.get(*p).map(|r| r.0).flatten(), ranges.get(*p).map(|r| r.1).flatten()))
        .collect();

    // 流吞吐（前端视角：含渲染排空）
    let stream_secs = match stream_range {
        (Some(s), Some(e)) => (e - s) as f64 / 1000.0,
        _ => 0.0,
    };
    let stream_mbps = if stream_secs > 0.0 {
        fe.stream_bytes as f64 / stream_secs / 1e6
    } else {
        0.0
    };

    // 隧道吞吐（快照差值）
    let snaps = TUNNEL_SNAPS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let snap = |key: &str| snaps.iter().find(|(k, _)| k == key).map(|(_, s)| *s);
    let tunnel_rate = |phase: &str, pick: fn(&stats::TunnelSnapshot) -> u64| -> f64 {
        match (snap(&format!("{phase}.start")), snap(&format!("{phase}.end"))) {
            (Some(a), Some(b)) if b.t_wall > a.t_wall => {
                (pick(&b) - pick(&a)) as f64 / ((b.t_wall - a.t_wall) as f64 / 1000.0) / 1e6
            }
            _ => 0.0,
        }
    };
    let up_mbps = tunnel_rate("load_up", |s| s.up_bytes);
    let down_mbps = tunnel_rate("load_down", |s| s.down_bytes);
    let churn_end = snap("churn.end");
    let churn_start = snap("churn.start");

    // channel 建立延迟
    let connect_lats: Vec<f64> = stats::CONNECT_LAT_US
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|us| *us as f64 / 1000.0)
        .collect();

    // 进程资源
    let samples = stats::PROC_SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let peak_self_rss = samples.iter().map(|s| s.self_rss).max().unwrap_or(0);
    let peak_wv_rss = samples.iter().map(|s| s.wv_rss).max().unwrap_or(0);
    let rss_growth = match (samples.first(), samples.last()) {
        (Some(a), Some(b)) => (b.self_rss as i64 + b.wv_rss as i64) - (a.self_rss as i64 + a.wv_rss as i64),
        _ => 0,
    };
    let cpu_during = |range: (Option<u64>, Option<u64>)| -> f64 {
        let (Some(s), Some(e)) = range else { return 0.0 };
        let xs: Vec<f32> = samples
            .iter()
            .filter(|p| p.t_wall >= s && p.t_wall < e)
            .map(|p| p.self_cpu + p.wv_cpu)
            .collect();
        if xs.is_empty() {
            0.0
        } else {
            (xs.iter().sum::<f32>() / xs.len() as f32) as f64
        }
    };

    // write 回调延迟（渲染积压）
    let cb_sorted = {
        let mut v = fe.cb_lat_ms.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v
    };

    // 按键延迟增幅：基线 vs 隧道负载
    let p99_of = |phase: &str| latency_by_phase.get(phase).and_then(|v| v.get("p99")).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let baseline_p99 = p99_of("baseline");
    let load_p99 = ["load_up", "load_down", "churn"]
        .iter()
        .map(|p| p99_of(p))
        .fold(0.0f64, f64::max);
    let p99_delta = load_p99 - baseline_p99;

    let summary = json!({
        "renderer": fe.renderer,
        "stream": {
            "mbps_frontend": (stream_mbps * 10.0).round() / 10.0,
            "secs_frontend": stream_secs,
            "write_cb_p99_ms": percentile(&cb_sorted, 99.0),
            "max_queue_kb": fe.max_queue_bytes / 1024,
        },
        "fps": {
            "stream": fps_summary(&fps_in(stream_range.0, stream_range.1)),
            "tunnel_load": fps_summary(&load_all),
            "overall": fps_summary(&fe.fps_buckets.iter().map(|b| b.fps).collect::<Vec<_>>()),
        },
        "keypress_latency_ms": latency_by_phase,
        "keypress_p99_delta_ms": p99_delta,
        "tunnel": {
            "up_mbps": (up_mbps * 10.0).round() / 10.0,
            "down_mbps": (down_mbps * 10.0).round() / 10.0,
            "churn_conns_total": churn_end.map(|s| s.total_conns).unwrap_or(0)
                - churn_start.map(|s| s.total_conns).unwrap_or(0),
            "churn_errors": churn_end.map(|s| s.connect_errors).unwrap_or(0)
                - churn_start.map(|s| s.connect_errors).unwrap_or(0),
            "channel_open_ms": lat_stats(&connect_lats),
        },
        "process": {
            "peak_self_rss_mb": peak_self_rss / 1024 / 1024,
            "peak_webview_rss_mb": peak_wv_rss / 1024 / 1024,
            "rss_growth_mb": rss_growth / 1024 / 1024,
            "cpu_avg_stream_pct": (cpu_during(stream_range) * 10.0).round() / 10.0,
            "cpu_avg_load_up_pct": (cpu_during(ranges.get("load_up").copied().unwrap_or_default()) * 10.0).round() / 10.0,
        },
    });

    let full = json!({
        "frontend": frontend,
        "backend": {
            "phase_events": *PHASE_EVENTS.lock().unwrap_or_else(|e| e.into_inner()),
            "loadgen": *LOADGEN_RESULTS.lock().unwrap_or_else(|e| e.into_inner()),
            "tunnel_snaps": snaps,
            "proc_samples": *samples,
        },
        "summary": summary,
    });

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("results");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(format!("run-{}.json", stats::now_wall_ms()));
    std::fs::write(&path, serde_json::to_string_pretty(&full).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    info!(?path, "report written");

    Ok(summary)
}

// ---------- 入口 ----------

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,russh=warn".into()),
        )
        .init();

    let state = AppState {
        client: tokio::sync::Mutex::new(None),
        echo_write: tokio::sync::Mutex::new(None),
        credits: Arc::new(Semaphore::new(CREDIT_HIGH)),
        outstanding: Arc::new(AtomicI64::new(0)),
        tunnels: tunnel::spawn_tunnel_thread(),
    };

    tauri::Builder::default()
        .manage(state)
        .setup(|_app| {
            stats::start_sampler();
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            open_echo,
            send_input,
            start_stream,
            credit,
            start_tunnel,
            stop_tunnels,
            run_load_sequence,
            finalize_report,
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            error!(?e, "tauri exited");
            std::process::exit(1);
        });
}
