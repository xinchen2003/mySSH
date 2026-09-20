#!/usr/bin/env node
/**
 * PR-0 性能基线采集：CDP 驱动运行中的 mySSH，取 perf_stats 快照存为 JSON artifact。
 *
 * 前置（应用须以 WebView2 远程调试端口启动）：
 *   set WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=9222
 *   npm run tauri dev            # 或直接运行已构建的 myssh.exe
 *
 * 用法：
 *   node scripts/perf-baseline.mjs [标签]
 *
 * 产物：perf/baseline-<时间戳>-<gitSha>[-标签].json（perf/ 已 gitignore，本地留存对比）
 * 零依赖：Node ≥22（内置 fetch / WebSocket）。
 */
import { execSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';

const CDP_PORT = process.env.MYSSH_CDP_PORT ?? '9222';
const label = process.argv[2] ? `-${process.argv[2].replace(/[^\w-]/g, '')}` : '';

async function main() {
  const targets = await (await fetch(`http://127.0.0.1:${CDP_PORT}/json`)).json();
  const page = targets.find((t) => t.type === 'page');
  if (!page) {
    throw new Error(
      `未找到 WebView 页面目标（127.0.0.1:${CDP_PORT}）——应用是否带 --remote-debugging-port 启动？`,
    );
  }

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

  const evalJson = async (expression) => {
    const r = await call('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (r.exceptionDetails) {
      const desc = r.exceptionDetails.exception?.description ?? r.exceptionDetails.text;
      throw new Error(`页面内执行失败: ${desc}`);
    }
    return r.result.value;
  };

  // dev 钩子（app/ui/src/dev.ts）→ perf_stats Tauri 命令
  const stats = await evalJson(
    `window.__myssh?.perfStats
       ? window.__myssh.perfStats()
       : Promise.reject(new Error('__myssh.perfStats 不存在——dev 钩子未注入或版本过旧'))`,
  );
  const appVersion = await evalJson(`window.__myssh.rawInvoke('app_version')`).catch(() => null);

  const gitRev = execSync('git rev-parse --short HEAD').toString().trim();
  const gitDirty = execSync('git status --porcelain').toString().trim().length > 0;

  const artifact = {
    capturedAt: new Date().toISOString(),
    gitRev,
    gitDirty,
    appVersion,
    stats,
  };
  mkdirSync('perf', { recursive: true });
  const ts = artifact.capturedAt.replace(/[:.]/g, '-');
  const file = `perf/baseline-${ts}-${gitRev}${label}.json`;
  writeFileSync(file, JSON.stringify(artifact, null, 2));
  console.log(`基线已写入 ${file}`);
  ws.close();
}

main().catch((e) => {
  console.error(`采集失败: ${e.message}`);
  process.exit(1);
});
