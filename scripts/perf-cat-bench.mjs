#!/usr/bin/env node
/**
 * PR-6 验收#8：高速 cat 吞吐对照驱动。
 * CDP 驱动运行中的 mySSH：连接本地 bench_stream sshd（shell 建立即全速下灌），
 * 以 xterm 缓冲中完成标记行 `bench-stream-done` 的首次出现为端到端完成信号，
 * 计算吞吐并落 JSON artifact（perf/cat-<标签>-<时间戳>.json）。
 *
 * 设计为对新旧二进制通用：只用既有 dev 钩子（connect/pendingHostKey/
 * answerHostKey/activeBufferText），不依赖 perf_stats 的 per-tab 字段。
 *
 * 前置：
 *   应用带调试端口启动（必须每次全新实例；GPU 黑名单会让 WebGL 回退软件渲染、
 *   终端吞吐塌缩数倍，故固定带 --ignore-gpu-blocklist）：
 *     set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222 --ignore-gpu-blocklist
 *   bench_stream 已起：cargo run -p core-ssh --example bench_stream -- 2324 <totalMiB>
 *
 * 用法：node scripts/perf-cat-bench.mjs <标签> [totalMiB=100] [cdpPort=9222] [sshPort=2324]
 */
import { mkdirSync, writeFileSync } from 'node:fs';

const label = process.argv[2] ?? 'run';
const totalMiB = Number(process.argv[3] ?? 100);
const CDP_PORT = process.argv[4] ?? '9222';
const SSH_PORT = Number(process.argv[5] ?? 2324);
const TOTAL_BYTES = totalMiB * 1024 * 1024;
const DONE_MARKER = 'bench-stream-done';
const TIMEOUT_MS = 10 * 60 * 1000;
const POLL_MS = 200;
async function main() {
  const targets = await (await fetch(`http://127.0.0.1:${CDP_PORT}/json`)).json();
  const page = targets.find((t) => t.type === 'page');
  if (!page) throw new Error(`未找到 WebView 页面目标（127.0.0.1:${CDP_PORT}）`);

  const ws = new WebSocket(page.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = () => reject(new Error('CDP WebSocket 连接失败'));
  });

  let seq = 0;
  const call = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++seq;
      const onMsg = (ev) => {
        const m = JSON.parse(ev.data);
        if (m.id !== id) return;
        ws.removeEventListener('message', onMsg);
        if (m.error) reject(new Error(m.error.message));
        else resolve(m.result);
      };
      ws.addEventListener('message', onMsg);
      ws.send(JSON.stringify({ id, method, params }));
    });

  const evalPage = async (expression) => {
    const r = await call('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      throw new Error(
        `页面内执行失败: ${r.exceptionDetails.exception?.description ?? r.exceptionDetails.text}`,
      );
    }
    return r.result.value;
  };

  const deadline = Date.now() + TIMEOUT_MS;

  // 0) 防污染闸：同实例复跑/旧 tab 未关会分流吞吐，要求 0 个预存 tab
  const preTabs = await evalPage(`window.__myssh.tabs().length`);
  if (preTabs > 0) {
    throw new Error(`存在 ${preTabs} 个已开 tab——请用全新应用实例测量`);
  }

  // 窗口置前：rAF 驱动的 drain 在窗口被遮挡时被 Chromium 节流，吞吐塌缩数倍
  await call('Page.bringToFront').catch(() => null);

  // 1) 建立终端连接（任意密码，bench_stream 全接受）
  await evalPage(
    `window.__myssh.connect({ host: '127.0.0.1', port: ${SSH_PORT}, user: 'bench', auth: { type: 'password', password: 'bench' } })`,
  );

  // 2) hostkey 弹窗应答（未知主机必弹）
  for (;;) {
    if (Date.now() > deadline) throw new Error('等待 hostkey 弹窗超时');
    if (await evalPage(`!!window.__myssh.pendingHostKey()`)) {
      await evalPage(`window.__myssh.answerHostKey(true, false)`);
      break;
    }
    await new Promise((r) => setTimeout(r, 150));
  }

  // 3) 轮询 xterm 缓冲：首行数据出现 = t0；完成标记出现 = t1
  let t0 = null;
  let t1 = null;
  let lastLen = 0;
  const samples = [];
  for (;;) {
    if (Date.now() > deadline) throw new Error('数据流超时未完成（疑似断流/断代）');
    const now = Date.now();
    // 只取缓冲末尾 4KB 文本查标记，避免全量 translate 拖累被测进程
    const { len, tail } = await evalPage(`(() => {
      const text = window.__myssh.activeBufferText() ?? '';
      return { len: text.length, tail: text.slice(-4096) };
    })()`);
    if (t0 === null && len > 0) t0 = now;
    lastLen = len;
    if (t0 !== null && samples.length % 10 === 0) samples.push({ t: now - t0, len });
    if (tail.includes(DONE_MARKER)) {
      t1 = now;
      break;
    }
    await new Promise((r) => setTimeout(r, POLL_MS));
  }
  if (t0 === null) throw new Error('未观测到任何数据');

  // 渲染器探针（此时 xterm 已存在）：无 canvas = DOM 渲染器（吞吐天花板低数倍）
  const renderer = await evalPage(
    `document.querySelectorAll('.xterm canvas').length > 0 ? 'canvas/webgl' : 'dom'`,
  );

  const elapsedMs = t1 - t0;
  const mibPerSec = TOTAL_BYTES / 1024 / 1024 / (elapsedMs / 1000);

  // 4) 可选健康快照：新协议二进制有 perfStats，断代必须为 false
  let health = null;
  try {
    const stats = await evalPage(
      `window.__myssh.perfStats ? window.__myssh.perfStats() : null`,
    );
    if (stats) {
      const sessions = stats.terminal?.sessions ?? [];
      health = {
        streamBroken: sessions.map((s) => s.streamBroken),
        outstandingBytes: sessions.map((s) => s.outstandingBytes),
      };
    }
  } catch {
    health = null;
  }

  const artifact = {
    capturedAt: new Date().toISOString(),
    label,
    renderer,
    totalBytes: TOTAL_BYTES,
    elapsedMs,
    mibPerSec: Math.round(mibPerSec * 100) / 100,
    bufferChars: lastLen,
    health,
    samples: samples.slice(-20),
  };
  mkdirSync('perf', { recursive: true });
  const file = `perf/cat-${label}-${artifact.capturedAt.replace(/[:.]/g, '-')}.json`;
  writeFileSync(file, JSON.stringify(artifact, null, 2));
  console.log(
    `完成: ${totalMiB} MiB / ${(elapsedMs / 1000).toFixed(2)}s = ${artifact.mibPerSec} MiB/s → ${file}`,
  );
  if (health?.streamBroken?.some(Boolean)) {
    console.error('警告: streamBroken=true（断代）');
    process.exit(2);
  }

  ws.close();
}

main().catch((e) => {
  console.error(`基准失败: ${e.message}`);
  process.exit(1);
});
