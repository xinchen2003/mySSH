#!/usr/bin/env node
/**
 * PR-18 性能门禁（PR 级）：跑全部自包含微基准 example，聚合 GATE 行与退出码。
 *
 * 用法：
 *   node scripts/perf-gate.mjs           # 全量门禁（任一不过 → 退出 1）
 *
 * 门禁项（各 example 内自检阈值，输出 `GATE <name> value=… threshold=… ok=…`）：
 *   core-tunnel --example gate_relay   隧道 relay 回环（median 延迟 / 吞吐）
 *   core-sftp   --example gate_sftp    TransferQueue 调度 + SFTP 读写回环
 *
 * 口径：纯内存/回环、release、取 median；阈值为本机首测 × 约 2 倍裕度——
 * 回归探测器，机器忙时数值上移属预期，不在共享 CI Runner 设硬失败（计划 PR-18 约束）。
 * Nightly/E2E 层（终端 flood、大文件、10 万小文件、千连接、soak）先出报告，
 * 稳定后再转硬门禁——见 scripts/perf-baseline.mjs 与 perf/ artifacts。
 */
import { spawnSync } from 'node:child_process';

const GATES = [
  ['core-tunnel', 'gate_relay'],
  ['core-sftp', 'gate_sftp'],
];

let failed = 0;
for (const [pkg, example] of GATES) {
  console.log(`\n=== ${pkg}/${example} ===`);
  const r = spawnSync(
    'cargo',
    ['run', '--release', '-p', pkg, '--example', example],
    { stdio: ['ignore', 'pipe', 'inherit'], shell: process.platform === 'win32' },
  );
  const out = r.stdout?.toString() ?? '';
  for (const line of out.split('\n')) {
    if (line.startsWith('GATE ')) console.log(line);
  }
  if (r.status !== 0) {
    failed++;
    console.error(`✗ ${example} 未过门禁（exit ${r.status}）`);
  }
}

if (failed > 0) {
  console.error(`\n性能门禁未通过：${failed}/${GATES.length} 项`);
  process.exit(1);
}
console.log('\n性能门禁全部通过');
