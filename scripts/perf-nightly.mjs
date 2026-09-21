#!/usr/bin/env node
/**
 * PR-18 Nightly 层：性能报告套件（报告态，不设硬失败）。
 *
 * 用法：
 *   node scripts/perf-nightly.mjs
 *
 * 产物：perf/nightly-<时间戳>-<gitSha>.json（perf/ 已 gitignore，本地/artifact 留存）
 *
 * 覆盖（自包含、可在任意机器跑）：
 *   - gate_relay / gate_sftp 微基准（GATE 行全量收录，不判失败）
 *   - bench_perf：RTT 矩阵 + 参数矩阵 + list 隔离（PR-11 验收行）
 *
 * 未覆盖（需专用 Runner/真机环境，记录在案不无限后拖）：
 *   - 终端 flood 端到端（spike 套件既有，见 CI perf-bench job）
 *   - 10 万小文件、1,000 隧道连接、内存峰值长跑、24h soak
 *     → 这些要真 sshd + 空闲机；脚本已留占位节，Runner 就绪后补实现。
 */
import { execSync, spawnSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';

const ts = new Date().toISOString().replace(/[:.]/g, '-');
const sha = execSync('git rev-parse --short HEAD').toString().trim();

function runBench(pkg, example) {
  const t0 = Date.now();
  const r = spawnSync('cargo', ['run', '--release', '-p', pkg, '--example', example], {
    stdio: ['ignore', 'pipe', 'pipe'],
    shell: process.platform === 'win32',
    timeout: 20 * 60 * 1000,
  });
  const stdout = r.stdout?.toString() ?? '';
  return {
    example,
    exitCode: r.status,
    wallMs: Date.now() - t0,
    gates: stdout.split('\n').filter((l) => l.startsWith('GATE ')),
    lines: stdout
      .split('\n')
      .filter((l) => /^(==|  RTT|\s+\d|baseline_p95|PR-11)/.test(l)),
    stderrTail: (r.stderr?.toString() ?? '').split('\n').slice(-5),
  };
}

const report = {
  ts,
  sha,
  host: { platform: process.platform, arch: process.arch },
  benches: [
    runBench('core-tunnel', 'gate_relay'),
    runBench('core-sftp', 'gate_sftp'),
    runBench('core-sftp', 'bench_perf'),
  ],
  placeholders: [
    'terminal-flood-e2e（spike 套件既有）',
    '100k-small-files（需专用 Runner）',
    '1000-tunnel-conns（需专用 Runner）',
    '24h-soak（需专用 Runner）',
  ],
};

mkdirSync('perf', { recursive: true });
const out = `perf/nightly-${ts}-${sha}.json`;
writeFileSync(out, JSON.stringify(report, null, 2));

// 控制台摘要
for (const b of report.benches) {
  console.log(`\n=== ${b.example} (exit ${b.exitCode}, ${(b.wallMs / 1000).toFixed(0)}s) ===`);
  for (const l of [...b.gates, ...b.lines]) console.log(l);
}
console.log(`\n报告：${out}`);
// 报告态：永不非零退出（硬门禁归 PR 级 perf-gate.mjs）
