/**
 * rAF 对齐的终端流消费器（spike 验证的背压链路，docs/design/04-dataflow.md）：
 *
 *   后端聚合帧 → push() 解帧头校验 epoch → 入队 → 每帧至多一次 write(batch, cb)
 *   → cb 触发后 credit(ackedTotal) 回传 → 后端信用闸放行更多数据
 *
 * 上一批未解析完则跳过本帧（规格书第 3 条：上一帧未 flush 完不得继续灌数据）。
 *
 * PR-6 累计 ACK 协议（docs/性能优化-修改计划.md + 约束 C4）：
 * - 帧自带 32B 头 {streamEpoch, frameSeq, startOffset, endOffset}（u64 LE）；
 * - credit 回传本批最大 endOffset（累计值），不再是批字节数；
 * - 旧 epoch 帧/重复帧丢弃；dispose 后迟到的 write 回调不回传
 *   （旧代际 callback 污染新流的客户端侧防线，后端 epoch 校验兜底）。
 */

/** 帧头长度：streamEpoch | frameSeq | startOffset | endOffset（各 u64 LE） */
export const FRAME_HEADER_LEN = 32;

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
    if (frame.length < FRAME_HEADER_LEN) return;
    const head = new DataView(frame.buffer, frame.byteOffset, FRAME_HEADER_LEN);
    const epoch = Number(head.getBigUint64(0, true));
    if (epoch !== this.streamEpoch) return; // 旧代际迟到帧：丢弃
    const endOffset = Number(head.getBigUint64(24, true));
    if (endOffset <= this.lastEndOffset) return; // 重复帧（防御）
    this.lastEndOffset = endOffset;
    const payload = frame.subarray(FRAME_HEADER_LEN);
    this.pending.push({ payload, endOffset });
    this.queued += payload.length;
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
