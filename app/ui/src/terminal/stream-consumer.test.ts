import { beforeEach, describe, expect, it, vi } from 'vitest';
import { StreamConsumer } from './stream-consumer';
import { FRAME_HEADER_LEN } from './stream-protocol';

/** 可控 rAF：收集回调手动驱动消费循环 */
let rafQueue: FrameRequestCallback[] = [];
function runRaf() {
  const q = rafQueue;
  rafQueue = [];
  for (const cb of q) cb(0);
}

/** 构造 PR-6 帧：32B 头 {streamEpoch, frameSeq, startOffset, endOffset}（u64 LE）+ payload */
function frame(epoch: number, seq: number, start: number, payloadLen: number): Uint8Array {
  const buf = new Uint8Array(FRAME_HEADER_LEN + payloadLen);
  const dv = new DataView(buf.buffer);
  dv.setBigUint64(0, BigInt(epoch), true);
  dv.setBigUint64(8, BigInt(seq), true);
  dv.setBigUint64(16, BigInt(start), true);
  dv.setBigUint64(24, BigInt(start + payloadLen), true);
  return buf;
}

describe('StreamConsumer PR-6 累计 ACK 协议', () => {
  beforeEach(() => {
    rafQueue = [];
    vi.stubGlobal('requestAnimationFrame', (cb: FrameRequestCallback) => {
      rafQueue.push(cb);
      return 0;
    });
  });

  it('解帧头：payload 进 xterm，credit 回传批末 endOffset（累计值而非字节数）', () => {
    const write = vi.fn((_data: Uint8Array, _cb: () => void) => undefined);
    const credit = vi.fn();
    const c = new StreamConsumer(7, write, credit);
    c.push(frame(7, 0, 0, 100));
    c.push(frame(7, 1, 100, 150));
    runRaf();
    // 两帧聚合为一批：250B payload（帧头不进 xterm）
    expect(write).toHaveBeenCalledTimes(1);
    expect(write.mock.calls[0][0].length).toBe(250);
    expect(c.queuedBytes).toBe(0);
    // xterm 消费完成 → 累计 ACK = 批末 endOffset
    write.mock.calls[0][1]();
    expect(credit).toHaveBeenCalledTimes(1);
    expect(credit).toHaveBeenCalledWith(250);
  });

  it('旧 epoch 帧丢弃（attach 换代后迟到帧不污染新流）', () => {
    const write = vi.fn((_data: Uint8Array, _cb: () => void) => undefined);
    const c = new StreamConsumer(7, write, vi.fn());
    c.push(frame(6, 0, 0, 100));
    runRaf();
    expect(write).not.toHaveBeenCalled();
    expect(c.queuedBytes).toBe(0);
  });

  it('重复/乱序帧不入队（endOffset 单调防御）', () => {
    const write = vi.fn((_data: Uint8Array, _cb: () => void) => undefined);
    const c = new StreamConsumer(7, write, vi.fn());
    c.push(frame(7, 0, 0, 100));
    c.push(frame(7, 0, 0, 100));
    runRaf();
    expect(write).toHaveBeenCalledTimes(1);
    expect(write.mock.calls[0][0].length).toBe(100);
  });

  it('gap 帧：记协议异常日志，payload 照常入队渲染（缺段不可恢复但不静默）', () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
    const write = vi.fn((_data: Uint8Array, _cb: () => void) => undefined);
    const c = new StreamConsumer(7, write, vi.fn());
    c.push(frame(7, 0, 0, 100));
    c.push(frame(7, 1, 200, 50)); // startOffset 200 ≠ lastEnd 100 → gap
    runRaf();
    expect(warn).toHaveBeenCalledTimes(1);
    expect(write).toHaveBeenCalledTimes(1);
    expect(write.mock.calls[0][0].length).toBe(150);
    warn.mockRestore();
  });

  it('dispose 后迟到的 write 回调不回传 credit（旧 callback 不污染新流）', () => {
    const write = vi.fn((_data: Uint8Array, _cb: () => void) => undefined);
    const credit = vi.fn();
    const c = new StreamConsumer(7, write, credit);
    c.push(frame(7, 0, 0, 100));
    runRaf();
    expect(write).toHaveBeenCalledTimes(1);
    c.dispose();
    // 旧 xterm write 回调晚到：不得触发任何 ACK
    write.mock.calls[0][1]();
    expect(credit).not.toHaveBeenCalled();
  });

  it('上一批未消费完则跳过本帧（rAF 背压语义保持）', () => {
    const write = vi.fn((_data: Uint8Array, _cb: () => void) => undefined);
    const c = new StreamConsumer(7, write, vi.fn());
    c.push(frame(7, 0, 0, 100));
    runRaf(); // write 已调但 cb 未触发 → flushing
    c.push(frame(7, 1, 100, 50));
    runRaf(); // flushing → 跳过
    expect(write).toHaveBeenCalledTimes(1);
    expect(c.queuedBytes).toBe(50);
  });
});
