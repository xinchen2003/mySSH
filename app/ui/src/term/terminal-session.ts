import { Channel, invoke } from '@tauri-apps/api/core';
import type { Terminal } from '@xterm/xterm';
import { createStreamChannel } from '../ipc/stream';
import { StreamConsumer } from '../terminal/stream-consumer';
import type { SessionStateFrame, TermEvent, TermOpenSpec } from './types';

/**
 * 单标签终端会话编排：channels 建立 → term_open → 输入/resize/credit 直发。
 *
 * 生命周期：TerminalView 挂载创建 xterm 后调 attach()；标签关闭调 close()。
 * 事件（session_state / hostkey_prompt / ki_challenge）经 onEvent 回调进 store。
 */
export class TerminalSession {
  /** 后端 tabId（term_open 返回后有效） */
  tabId: string | null = null;
  /** TerminalView 的旁路钩子：session_state 帧写重连/关闭标记进 xterm */
  private stateHook: ((ev: SessionStateFrame) => void) | null = null;
  private consumer: StreamConsumer | null = null;
  private encoder = new TextEncoder();

  constructor(private readonly onEvent: (ev: TermEvent) => void) {}

  async attach(
    term: Terminal,
    spec: TermOpenSpec,
    stateHook?: (ev: SessionStateFrame) => void,
  ): Promise<string> {
    this.stateHook = stateHook ?? null;
    // 消费器必须先于 term_open 就绪：shell banner 可能紧随返回抵达
    this.consumer = new StreamConsumer(
      (chunk, cb) => term.write(chunk, cb),
      (bytes) => {
        if (this.tabId) void invoke('term_credit', { tabId: this.tabId, bytes });
      },
    );
    const data = createStreamChannel((frame) => this.consumer?.push(frame));
    const events = new Channel<TermEvent>();
    events.onmessage = (ev) => {
      this.onEvent(ev);
      if (ev.type === 'session_state') this.stateHook?.(ev);
    };

    const res = await invoke<{ tabId: string }>('term_open', {
      spec,
      data,
      events,
      cols: term.cols,
      rows: term.rows,
    });
    this.tabId = res.tabId;

    // 输入零聚合直发（规格书输入路径预算）
    term.onData((s) => {
      if (!this.tabId) return;
      void invoke('term_input', {
        tabId: this.tabId,
        bytes: Array.from(this.encoder.encode(s)),
      });
    });
    term.onResize(({ cols, rows }) => {
      if (this.tabId) void invoke('term_resize', { tabId: this.tabId, cols, rows });
    });
    return this.tabId;
  }

  async close(): Promise<void> {
    const id = this.tabId;
    this.tabId = null;
    this.consumer?.dispose();
    this.consumer = null;
    if (id) await invoke('term_close', { tabId: id });
  }
}
