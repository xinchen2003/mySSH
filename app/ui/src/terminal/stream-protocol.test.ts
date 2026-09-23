import { describe, expect, it } from 'vitest';
import corpusText from '../../../../fixtures/stream-protocol.jsonl?raw';
import { parseFrame } from './stream-protocol';

interface DecodeStep {
  hex: string;
  expect: 'accept' | 'drop';
  endOffset?: number;
  gap?: boolean;
  reason?: string;
}
interface CorpusLine {
  kind: string;
  name?: string;
  streamEpoch?: number;
  steps?: DecodeStep[];
}

function hexToBytes(hex: string): Uint8Array {
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  return out;
}

// decode 用例由 TS 跑；encode/ack 由 Rust 侧跑（两侧各跳过不拥有的 kind）
describe('stream-protocol golden 语料（fixtures/stream-protocol.jsonl，与 Rust 共用）', () => {
  const lines = corpusText.split('\n').filter(Boolean);
  for (const line of lines) {
    const c = JSON.parse(line) as CorpusLine;
    if (c.kind !== 'decode' || !c.name || c.streamEpoch === undefined || !c.steps) continue;
    it(c.name, () => {
      let lastEnd = 0;
      for (const step of c.steps ?? []) {
        const got = parseFrame(hexToBytes(step.hex), c.streamEpoch ?? 0, lastEnd);
        if (step.expect === 'drop') {
          expect(got).toEqual({ kind: 'drop', reason: step.reason });
          continue;
        }
        expect(got.kind).toBe('accept');
        if (got.kind !== 'accept') return;
        expect(got.endOffset).toBe(step.endOffset);
        expect(got.gap).toBe(step.gap ?? false);
        lastEnd = got.endOffset;
      }
    });
  }
});
