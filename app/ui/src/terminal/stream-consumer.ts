/**
 * rAF 对齐的终端流消费器（spike 验证的背压链路，docs/design/04-dataflow.md）：
 *
 *   后端聚合帧 → push() 入队 → 每帧至多一次 write(batch, cb)
 *   → cb 触发后 credit(len) 回传 → 后端信用闸放行更多数据
 *
 * 上一批未解析完则跳过本帧（规格书第 3 条：上一帧未 flush 完不得继续灌数据）。
 */
export class StreamConsumer {
  private pending: Uint8Array[] = [];
  private flushing = false;
  private disposed = false;

  constructor(
    private readonly write: (data: Uint8Array, cb: () => void) => void,
    private readonly credit: (bytes: number) => void,
  ) {
    const drain = () => {
      if (this.disposed) return;
      this.flushOnce();
      requestAnimationFrame(drain);
    };
    requestAnimationFrame(drain);
  }

  push(frame: Uint8Array): void {
    this.pending.push(frame);
  }

  /** 当前排队字节数（可观测性；UI 状态指示用） */
  get queuedBytes(): number {
    return this.pending.reduce((sum, p) => sum + p.length, 0);
  }

  private flushOnce(): void {
    if (this.pending.length === 0 || this.flushing) return;
    this.flushing = true;
    const batch = concatParts(this.pending);
    this.pending = [];
    this.write(batch, () => {
      this.flushing = false;
      this.credit(batch.length);
    });
  }

  dispose(): void {
    this.disposed = true;
    this.pending = [];
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
