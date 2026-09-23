/**
 * rAF 对齐的终端流消费器（spike 验证的背压链路，docs/design/04-dataflow.md）：
 *
 *   后端聚合帧 → push()（解帧/校验在 stream-protocol.ts）→ 入队 → 每帧至多一次
 *   write(batch, cb) → cb 触发后 credit(ackedTotal) 回传 → 后端信用闸放行更多数据
 *
 * 上一批未解析完则跳过本帧（规格书第 3 条：上一帧未 flush 完不得继续灌数据）。
 * dispose 后迟到的 write 回调不回传 credit（旧代际 callback 污染新流的客户端侧防线，
 * 后端 epoch 校验兜底）。
 */

import { parseFrame } from './stream-protocol';

export class StreamConsumer {
  private pending: { payload: Uint8Array; endOffset: number }[] = [];
  private queued = 0;
  private flushing = false;
  private disposed = false;
  /** 已入队帧的最大 endOffset：代际内 offset 单调，重复/乱序帧防御 */
  private lastEndOffset = 0;

  constructor(
    private readonly streamEpoch: number,
    private readonly write: (data: Uint8Array, cb: () => void) => void,
    private readonly credit: (ackedTotal: number) => void,
  ) {
    const drain = () => {
      if (this.disposed) return;
      this.flushOnce();
      requestAnimationFrame(drain);
    };
    requestAnimationFrame(drain);
  }

  push(frame: Uint8Array): void {
    const parsed = parseFrame(frame, this.streamEpoch, this.lastEndOffset);
    if (parsed.kind === 'drop') return;
    if (parsed.gap) {
      // gap = 字节流缺段：缺段不可恢复，payload 照常渲染，记协议异常
      console.warn(
        `[stream] 帧间隙：startOffset 与已入队 endOffset 不连续（epoch=${this.streamEpoch}, endOffset=${parsed.endOffset}）`,
      );
    }
    this.lastEndOffset = parsed.endOffset;
    this.pending.push({ payload: parsed.payload, endOffset: parsed.endOffset });
    this.queued += parsed.payload.length;
  }

  /** 当前排队字节数（可观测性；整数维护，push/flush O(1)） */
  get queuedBytes(): number {
    return this.queued;
  }

  private flushOnce(): void {
    if (this.pending.length === 0 || this.flushing) return;
    this.flushing = true;
    const batchEnd = this.pending[this.pending.length - 1].endOffset;
    const batch = concatParts(this.pending.map((p) => p.payload));
    this.pending = [];
    this.queued -= batch.length;
    this.write(batch, () => {
      this.flushing = false;
      if (!this.disposed) this.credit(batchEnd);
    });
  }

  dispose(): void {
    this.disposed = true;
    this.pending = [];
    this.queued = 0;
  }
}

function concatParts(parts: Uint8Array[]): Uint8Array {
  if (parts.length === 1) return parts[0];
  const total = parts.reduce((sum, p) => sum + p.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const p of parts) {
    out.set(p, offset);
    offset += p.length;
  }
  return out;
}
