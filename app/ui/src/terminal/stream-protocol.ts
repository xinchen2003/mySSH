/**
 * 终端流协议 TS adapter（PR-6 累计 ACK 协议的解码/校验侧，卡 1）：
 * Rust 侧 module：crates/app/src/stream_protocol.rs（编码 + 信用闸 + ACK 语义）；
 * 契约文档：docs/design/03-ipc-contract.md §帧格式；字节级钉版：fixtures/stream-protocol.jsonl
 * （两侧测试共用同一语料，漂移即红）。
 *
 * 帧头 32B（u64 LE）：streamEpoch | frameSeq(保留，不校验) | startOffset | endOffset。
 * 校验规则：旧 epoch 帧丢弃；endOffset 不递增 = 重复帧丢弃；
 * startOffset 与已入队 endOffset 不连续 = gap（协议异常：字节流缺段不可恢复，
 * payload 照常渲染并由调用方记日志）。
 */

/** 帧头长度：streamEpoch | frameSeq | startOffset | endOffset（各 u64 LE） */
export const FRAME_HEADER_LEN = 32;

export type FrameParse =
  | { kind: 'accept'; payload: Uint8Array; endOffset: number; gap: boolean }
  | { kind: 'drop'; reason: 'too-short' | 'old-epoch' | 'duplicate' };

/**
 * 解帧头并校验代际/连续性。`lastEndOffset` = 本代际已入队帧的最大 endOffset
 * （首帧传 0；代际内 offset 从 0 起单调连续）。
 */
export function parseFrame(
  frame: Uint8Array,
  streamEpoch: number,
  lastEndOffset: number,
): FrameParse {
  if (frame.length < FRAME_HEADER_LEN) return { kind: 'drop', reason: 'too-short' };
  const head = new DataView(frame.buffer, frame.byteOffset, FRAME_HEADER_LEN);
  const epoch = Number(head.getBigUint64(0, true));
  if (epoch !== streamEpoch) return { kind: 'drop', reason: 'old-epoch' };
  const startOffset = Number(head.getBigUint64(16, true));
  const endOffset = Number(head.getBigUint64(24, true));
  if (endOffset <= lastEndOffset) return { kind: 'drop', reason: 'duplicate' };
  return {
    kind: 'accept',
    payload: frame.subarray(FRAME_HEADER_LEN),
    endOffset,
    gap: startOffset !== lastEndOffset,
  };
}
