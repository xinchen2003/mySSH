// PR-9 事件协议：applyTransferEvents / trimTerminal 纯函数测试。
// 验收映射：乱序/重复事件不破坏状态；序号缺口可检测（重建触发）；
// 代际不符丢弃；store 终态条目有界。
import { describe, expect, it } from 'vitest';
import type { TransferJobView, TransferView } from '../term/types';
import { applyTransferEvents, trimTerminal, type TransferEventJson } from './transfer-store';

const G = 42;

function tv(id: string, state: TransferView['state'] = 'running'): TransferView {
  return {
    id,
    direction: 'upload',
    local: `/l/${id}`,
    remote: `/r/${id}`,
    state,
    bytesDone: 10,
    bytesTotal: 100,
    retries: 0,
    error: null,
  };
}

function jv(id: string, state: TransferJobView['state'] = 'transferring'): TransferJobView {
  return {
    id,
    direction: 'download',
    summary: `job ${id}`,
    state,
    paused: false,
    scanDone: true,
    discoveredFiles: 10,
    discoveredBytes: 1000,
    completedFiles: 5,
    failedFiles: 0,
    skipped: 0,
    bytesDone: 500,
    error: null,
    current: [],
    failedEntries: [],
    rate: 0,
  };
}

function ev(
  id: string,
  kind: 'transfer' | 'job',
  seq: number,
  payload: TransferView | TransferJobView | null,
  eventType: 'upsert' | 'remove' = payload ? 'upsert' : 'remove',
): TransferEventJson {
  return { id, kind, eventType, generation: G, eventSeq: seq, payload };
}

describe('applyTransferEvents', () => {
  it('upsert 追加与全量替换', () => {
    const seqs: Record<string, number> = {};
    const r1 = applyTransferEvents(
      { transfers: [], jobs: [] },
      seqs,
      [ev('t:a', 'transfer', 1, tv('a'))],
      G,
    );
    expect(r1.ok && r1.transfers.map((t) => t.id)).toEqual(['a']);
    // 同 id 再 upsert → 全量替换（状态推进）
    const r2 = applyTransferEvents(
      { transfers: r1.ok ? r1.transfers : [], jobs: [] },
      seqs,
      [ev('t:a', 'transfer', 2, tv('a', 'done'))],
      G,
    );
    expect(r2.ok && r2.transfers).toHaveLength(1);
    expect(r2.ok && r2.transfers[0].state).toBe('done');
  });

  it('remove 按 id 删除；删除不存在无害', () => {
    const seqs: Record<string, number> = {};
    const cur = { transfers: [tv('a'), tv('b')], jobs: [] };
    // 注意：直接构造的列表不进 seqs——remove 序列从 1 起
    const r = applyTransferEvents(cur, seqs, [ev('t:a', 'transfer', 1, null)], G);
    expect(r.ok && r.transfers.map((t) => t.id)).toEqual(['b']);
    const r2 = applyTransferEvents(cur, { 't:zzz': 0 }, [ev('t:zzz', 'transfer', 1, null)], G);
    expect(r2.ok && r2.transfers).toHaveLength(2);
  });

  it('重复/迟到事件丢弃（eventSeq <= last 不破坏状态）', () => {
    const seqs: Record<string, number> = { 't:a': 5 };
    const cur = { transfers: [tv('a', 'done')], jobs: [] };
    const r = applyTransferEvents(cur, seqs, [ev('t:a', 'transfer', 3, tv('a', 'running'))], G);
    expect(r.ok).toBe(true);
    expect(r.ok && r.transfers[0].state).toBe('done'); // 旧事件未覆盖新状态
    expect(seqs['t:a']).toBe(5);
  });

  it('序号缺口 → gap（调用方 snapshot 重同步）', () => {
    const seqs: Record<string, number> = { 't:a': 1 };
    const r = applyTransferEvents(
      { transfers: [], jobs: [] },
      seqs,
      [ev('t:a', 'transfer', 3, tv('a'))],
      G,
    );
    expect(r).toEqual({ ok: false, reason: 'gap' });
  });

  it('代际不符 → stale-generation', () => {
    const r = applyTransferEvents(
      { transfers: [], jobs: [] },
      {},
      [{ ...ev('t:a', 'transfer', 1, tv('a')), generation: 999 }],
      G,
    );
    expect(r).toEqual({ ok: false, reason: 'stale-generation' });
  });

  it('job 事件与 transfer 事件分流', () => {
    const seqs: Record<string, number> = {};
    const r = applyTransferEvents(
      { transfers: [], jobs: [] },
      seqs,
      [ev('j:x', 'job', 1, jv('x')), ev('j:x', 'job', 2, jv('x', 'completed'))],
      G,
    );
    expect(r.ok && r.jobs).toHaveLength(1);
    expect(r.ok && r.jobs[0].state).toBe('completed');
    expect(r.ok && r.transfers).toHaveLength(0);
  });
});

describe('trimTerminal', () => {
  it('保留全部非终态 + 末尾 cap 条终态', () => {
    const terminal = new Set(['done', 'failed']);
    const list = [
      ...Array.from({ length: 5 }, (_, i) => ({ id: `d${i}`, state: 'done' })),
      { id: 'run', state: 'running' },
      ...Array.from({ length: 5 }, (_, i) => ({ id: `f${i}`, state: 'failed' })),
    ];
    const out = trimTerminal(list, terminal, 3);
    expect(out.filter((t) => t.state === 'running')).toHaveLength(1);
    expect(out.filter((t) => terminal.has(t.state))).toHaveLength(3);
    // 留尾部终态（f2/f3/f4），头部旧终态被裁；相对顺序保持
    expect(out.map((t) => t.id)).toEqual(['run', 'f2', 'f3', 'f4']);
    expect(out.some((t) => t.id === 'd0')).toBe(false);
  });

  it('未超 cap 原样返回', () => {
    const list = [
      { id: 'a', state: 'done' },
      { id: 'b', state: 'running' },
    ];
    expect(trimTerminal(list, new Set(['done']), 5)).toBe(list);
  });
});
