/**
 * Spike-1/2 前端：终端数据通路 + 测量编排。
 *
 * 数据通路（规格书 1-4 条）：
 *   后端 8ms/256KB 聚合 → ipc::Channel(Raw) → 本队列 → rAF 对齐的 write(data, cb)
 *   → cb 触发后 invoke credit 回传字节数（背压信用，驱动后端暂停 SSH 读取）
 *
 * 测量：
 *   - fps：rAF 计数，1s 桶
 *   - 按键回显延迟：10Hz 单字节 echo 探针，全周期连续采样
 *   - 流吞吐：stream 阶段字节数 / 壁钟时间
 *   - write 回调延迟：入队到 xterm 解析完成的时间（渲染积压指标）
 */
import { Terminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { FitAddon } from '@xterm/addon-fit';
import { invoke, Channel } from '@tauri-apps/api/core';
import '@xterm/xterm/css/xterm.css';

// ---------- 与后端的事件契约
type StatsEvent =
  | { type: 'stream_done'; bytes: number; secs: number; tWall: number }
  | { type: 'phase'; name: string; boundary: 'start' | 'end'; tWall: number }
  | { type: 'all_done' }
  | { type: 'log'; msg: string };

interface PhaseRange {
  start?: number;
  end?: number;
}

interface RttSample {
  t: number;
  rtt: number;
}

// ---------- 时钟对齐：performance.now() ↔ Date.now()
const t0Wall = Date.now();
const t0Perf = performance.now();
const wallOf = (perf: number) => Math.round(t0Wall + (perf - t0Perf));

// ---------- HUD
const hud = document.getElementById('hud')!;
const live = document.getElementById('live')!;
const log = (m: string) => {
  hud.textContent += `[${((performance.now() - t0Perf) / 1000).toFixed(1)}s] ${m}\n`;
  hud.scrollTop = hud.scrollHeight;
  try {
    invoke('log_frontend', { msg: m }).catch(() => {});
  } catch {
    // 非 Tauri 环境（浏览器诊断）：invoke 同步抛错，忽略
  }
};
window.addEventListener('error', (e) => log(`JSERROR: ${e.message}`));
window.addEventListener('unhandledrejection', (e) => log(`JSREJECT: ${String(e.reason)}`));

function sleep(ms: number): Promise<void> {
  const { promise, resolve } = Promise.withResolvers<void>();
  setTimeout(resolve, ms);
  return promise;
}

// ---------- 终端
const term = new Terminal({ scrollback: 10_000, allowProposedApi: true });
const fit = new FitAddon();
term.loadAddon(fit);
term.open(document.getElementById('term')!);
fit.fit();
let renderer = 'webgl';
try {
  term.loadAddon(new WebglAddon());
} catch (e) {
  renderer = 'canvas-fallback';
  log(`webgl unavailable, fell back: ${e}`);
}
log(`renderer: ${renderer}`);

// ---------- FPS 桶
let frames = 0;
const fpsBuckets: { t: number; fps: number }[] = [];
(function raf() {
  frames++;
  requestAnimationFrame(raf);
})();
setInterval(() => {
  fpsBuckets.push({ t: wallOf(performance.now()), fps: frames });
  frames = 0;
}, 1000);

// ---------- echo 延迟探针（10Hz，全程连续）
const rtts: RttSample[] = [];
const probeOutstanding: number[] = [];
const echoCh = new Channel<ArrayBuffer>();
echoCh.onmessage = (buf) => {
  const bytes = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
  term.write(bytes);
  const now = performance.now();
  for (let i = 0; i < bytes.length; i++) {
    const t0 = probeOutstanding.shift();
    if (t0 !== undefined) rtts.push({ t: wallOf(now), rtt: now - t0 });
  }
};

// ---------- 流数据路径
const streamCh = new Channel<ArrayBuffer>();
let pending: Uint8Array[] = [];
let pendingBytes = 0;
let flushing = false;
let streamBytes = 0;
let maxQueueBytes = 0;
const cbLat: number[] = [];

streamCh.onmessage = (buf) => {
  const b = buf instanceof Uint8Array ? buf : new Uint8Array(buf);
  pending.push(b);
  pendingBytes += b.length;
  streamBytes += b.length;
  if (pendingBytes > maxQueueBytes) maxQueueBytes = pendingBytes;
};

function concatParts(parts: Uint8Array[]): Uint8Array {
  if (parts.length === 1) return parts[0];
  const n = parts.reduce((s, p) => s + p.length, 0);
  const out = new Uint8Array(n);
  let o = 0;
  for (const p of parts) {
    out.set(p, o);
    o += p.length;
  }
  return out;
}

// rAF 对齐排空：上一批未解析完（cb 未触发）则跳过本帧 —— 背压点
function drain() {
  if (pending.length && !flushing) {
    flushing = true;
    const batch = concatParts(pending);
    pending = [];
    pendingBytes = 0;
    const t0 = performance.now();
    term.write(batch, () => {
      cbLat.push(performance.now() - t0);
      flushing = false;
      invoke('credit', { bytes: batch.length });
    });
  }
  requestAnimationFrame(drain);
}
requestAnimationFrame(drain);

// ---------- 后端事件
// 注意：一个 JS Channel 只能传给一个命令。Rust 侧 Channel 在任务结束 Drop 时会发
// end 标记，且每个 Rust Channel 实例的消息序号从 0 重新计数 —— 跨命令复用同一
// JS Channel 会导致后续消息被 JS 侧当作乱序旧消息永久搁置（本次实测踩中）。
const mkStatsChannel = () => {
  const ch = new Channel<StatsEvent>();
  ch.onmessage = onStatsEvent;
  return ch;
};

const phases: Record<string, PhaseRange> = {};
let allDone = false;
let streamDoneInfo: Extract<StatsEvent, { type: 'stream_done' }> | null = null;

function onStatsEvent(ev: StatsEvent) {
  switch (ev.type) {
    case 'stream_done':
      streamDoneInfo = ev;
      phases.stream.end = ev.tWall;
      log(`stream done: ${(ev.bytes / 1e6).toFixed(1)}MB in ${ev.secs.toFixed(2)}s (backend-observed)`);
      break;
    case 'phase':
      phases[ev.name] = { ...phases[ev.name], [ev.boundary]: ev.tWall };
      log(`phase ${ev.name}.${ev.boundary}`);
      break;
    case 'all_done':
      allDone = true;
      log('load sequence done');
      break;
    case 'log':
      log(ev.msg);
      break;
  }
}

async function waitFor(pred: () => boolean, timeoutMs: number, what: string) {
  const t0 = Date.now();
  while (!pred()) {
    if (Date.now() - t0 > timeoutMs) throw new Error(`timeout waiting ${what}`);
    await sleep(100);
  }
}

setInterval(() => {
  const lastFps = fpsBuckets.length ? fpsBuckets[fpsBuckets.length - 1].fps : '-';
  live.textContent = `fps(1s)=${lastFps} | rtts=${rtts.length} | streamMB=${(streamBytes / 1e6).toFixed(1)} | queueKB=${(pendingBytes / 1024).toFixed(0)}`;
}, 500);

async function main() {
  await invoke('connect');
  log('ssh connected (interactive)');
  await invoke('open_echo', { data: echoCh });
  setInterval(() => {
    probeOutstanding.push(performance.now());
    invoke('send_input', { bytes: [0x55] });
  }, 100);
  log('echo probes running @10Hz');

  await sleep(5000); // 基线期
  phases.baseline = { start: t0Wall, end: Date.now() };

  // 阶段 A：100MB 流（对应 cat 100MB）
  phases.stream = { start: Date.now() };
  await invoke('start_stream', { data: streamCh, stats: mkStatsChannel(), sizeMb: 100 });
  await waitFor(() => streamDoneInfo !== null, 180_000, 'stream_done');
  // 等前端队列排空（所有 credit 回流）
  await waitFor(() => pendingBytes === 0 && !flushing, 60_000, 'drain');
  phases.stream.end = Date.now();
  log(`stream drained at frontend: ${(streamBytes / 1e6).toFixed(1)}MB`);

  await sleep(3000); // 静置期
  phases.idle2 = { start: phases.stream.end, end: Date.now() };

  // 阶段 B：隧道 + 负载（后端自驱动 loadgen 序列）
  await invoke('start_tunnel', { listenPort: 13389, targetPort: 9999 });
  await invoke('start_tunnel', { listenPort: 13390, targetPort: 9998 });
  log('tunnels up (13389→sink, 13390→source)');
  await invoke('run_load_sequence', { stats: mkStatsChannel() });
  await waitFor(() => allDone, 300_000, 'all_done');

  await invoke('stop_tunnels');
  await sleep(1000);

  const frontend = {
    renderer,
    startedWall: t0Wall,
    phases,
    rtts,
    fpsBuckets,
    streamBytes,
    maxQueueBytes,
    cbLatMs: cbLat,
  };
  const summary: unknown = await invoke('finalize_report', { frontend });
  log('==== SUMMARY ====');
  log(JSON.stringify(summary, null, 2));
}

main().catch((e: unknown) => {
  const msg = e instanceof Error ? e.message : String(e);
  log(`FATAL: ${msg}`);
});
