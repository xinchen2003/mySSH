import { Channel, invoke } from '@tauri-apps/api/core';
import type { Terminal } from '@xterm/xterm';
import { createStreamChannel } from '../ipc/stream';
import { StreamConsumer } from '../terminal/stream-consumer';
import type { ConnectTarget, SessionStateFrame, TermEvent } from './types';

/**
 * 单标签终端会话编排：channels 建立 → term_open → 输入/resize/credit 直发。
 *
 * 生命周期：TerminalView 挂载创建 xterm 后调 attach()；标签关闭调 close()。
 * 事件（session_state / hostkey_prompt / ki_challenge）经 onEvent 回调进 store。
 */
export class TerminalSession {
  /** 后端 tabId（term_open 返回后有效） */
  tabId: string | null = null;
  /** 终端 cwd（OSC 7 上报；shell 未开集成时为 null） */
  cwd: string | null = null;
  /** TerminalView 的旁路钩子：session_state 帧写重连/关闭标记进 xterm */
  private stateHook: ((ev: SessionStateFrame) => void) | null = null;
  private consumer: StreamConsumer | null = null;
  /** 开链前 write 的缓冲队列（见 write） */
  private pendingWrites: string[] = [];
  private encoder = new TextEncoder();
  /** 输入/resize 订阅释放器：attach 幂等关键——原位重连重挂前必须先释放上一轮 */
  private disposers: { dispose(): void }[] = [];
  /** 广播输入旁路钩子（11）：onData 直发后同步触发；未开链的输入不触发 */
  inputHook: ((data: string) => void) | null = null;

  constructor(readonly onEvent: (ev: TermEvent) => void) {}

  async attach(
    term: Terminal,
    target: ConnectTarget,
    stateHook?: (ev: SessionStateFrame) => void,
  ): Promise<string> {
    this.stateHook = stateHook ?? null;
    // 原位重连幂等：同一 xterm 实例重复 attach 时，先释放上一轮输入订阅与消费器，
    // 否则 onData/onResize 会注册两次导致输入重复发送
    this.detachInput();
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

    // term_open 可能耗时数秒（WAN 首连 + hostkey 确认），期间 fit 可能改尺寸；
    // 记下开链尺寸，连接建立后比对补发 resize（onResize 注册前的变更会丢）
    const openedCols = term.cols;
    const openedRows = term.rows;
    const res = await invoke<{ tabId: string }>('term_open', {
      spec: target.kind === 'spec' ? target.spec : null,
      sessionId: target.kind === 'session' ? target.sessionId : null,
      // 终端编码：档案会话取建档快照（缺省由后端读档案），内联 spec 取其自身字段
      encoding: target.kind === 'spec' ? (target.spec.encoding ?? null) : (target.encoding ?? null),
      data,
      events,
      cols: openedCols,
      rows: openedRows,
    });
    this.tabId = res.tabId;
    // 补发开链前缓冲的写入
    for (const s of this.pendingWrites.splice(0)) this.write(s);

    // OSC 7：shell 上报 cwd（file://host/path），SFTP 面板「跟随终端」用
    term.parser.registerOscHandler(7, (data) => {
      const m = /^file:\/\/[^/]*(\/.*)$/.exec(data);
      if (m) {
        try {
          this.cwd = decodeURIComponent(m[1]);
        } catch {
          this.cwd = m[1];
        }
      }
      return true;
    });
    if (term.cols !== openedCols || term.rows !== openedRows)
      void invoke('term_resize', { tabId: res.tabId, cols: term.cols, rows: term.rows });

    // 输入零聚合直发（规格书输入路径预算）；广播钩子在同窗口同步扇出
    this.disposers.push(
      term.onData((s) => {
        if (!this.tabId) return;
        this.write(s);
        this.inputHook?.(s);
      }),
      term.onResize(({ cols, rows }) => {
        if (this.tabId) void invoke('term_resize', { tabId: this.tabId, cols, rows });
      }),
    );
    return this.tabId;
  }

  /** 发送一段输入（广播扇出用；OSC 7 钩子/分屏 cd 注入已移除——不向远程 shell 注入字节）。
   *  connected 事件可能先于 term_open 返回抵达（tabId 未赋值），
   *  此时入队缓冲，开链后按序补发；close 时清空。 */
  write(s: string): void {
    if (!this.tabId) {
      if (this.pendingWrites.length < 64) this.pendingWrites.push(s);
      return;
    }
    void invoke('term_input', {
      tabId: this.tabId,
      bytes: Array.from(this.encoder.encode(s)),
    });
  }

  /** 释放输入/resize 订阅与消费器（OSC 7 handler 重注册即覆盖，无需释放） */
  private detachInput(): void {
    for (const d of this.disposers) d.dispose();
    this.disposers = [];
    this.consumer?.dispose();
    this.consumer = null;
  }

  async close(): Promise<void> {
    const id = this.tabId;
    this.tabId = null;
    this.pendingWrites = [];
    this.detachInput();
    if (id) await invoke('term_close', { tabId: id });
  }
}
