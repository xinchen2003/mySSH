import { Channel } from '@tauri-apps/api/core';

/**
 * 打开一条数据流通道。
 *
 * 不变量（spike 踩坑 #2）：一个 JS Channel 只能传给一个后端命令——
 * Rust 侧 Channel 在任务结束 Drop 时发送 end 标记且消息序号从 0 重计，
 * 跨命令复用同一 JS Channel 会让后续消息被当作乱序旧消息永久搁置。
 * 因此每条流都经此工厂新建实例。
 */
export function createStreamChannel(onFrame: (frame: Uint8Array) => void): Channel<ArrayBuffer> {
  const ch = new Channel<ArrayBuffer>();
  ch.onmessage = (buf) => onFrame(buf instanceof Uint8Array ? buf : new Uint8Array(buf));
  return ch;
}
