import { Channel, invoke } from '@tauri-apps/api/core';
import type { Terminal } from '@xterm/xterm';
import { createStreamChannel } from '../ipc/stream';
import { StreamConsumer } from '../terminal/stream-consumer';
import { bytesToB64, createTransferFilter, type TransferFilter } from './file-transfer';
import type { NotificationLevel } from '../state/app-store';
import type { ConnectTarget, SessionStateFrame, TermEvent } from './types';

/** 新数据通道代际标识（PR-6）：53bit 随机数（JSON 安全整数），每次 attach 必不同 */
function newStreamEpoch(): number {
  const buf = new Uint32Array(2);
  crypto.getRandomValues(buf);
  return (buf[0] & 0x1fffff) * 2 ** 32 + buf[1];
}

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
  /** 终端内文件传输（ZMODEM/trzsz）：仅 UTF-8 会话挂载；GBK 会话为 null */
  private transfer: TransferFilter | null = null;
  /** 前台标记（PR-17 二期）：后台 tab 由后端 ring buffer 接管；开链前变更挂起，
   *  tabId 赋值后补同步（与 pendingWrites 同手法） */
  private focused = true;
  /** 传输结果 toast 透传（app-store notify）；缺省丢弃 */
  private readonly notify: (msg: string, level?: NotificationLevel) => void;

  constructor(
    readonly onEvent: (ev: TermEvent) => void,
    notify?: (msg: string, level?: NotificationLevel) => void,
  ) {
    this.notify = notify ?? (() => undefined);
  }

  async attach(
    term: Terminal,
    target: ConnectTarget,
    stateHook?: (ev: SessionStateFrame) => void,
  ): Promise<string> {
    this.stateHook = stateHook ?? null;
    // 原位重连幂等：同一 xterm 实例重复 attach 时，先释放上一轮输入订阅与消费器，
    // 否则 onData/onResize 会注册两次导致输入重复发送
    this.detachInput();
    // 消费器必须先于 term_open 就绪：shell banner 可能紧随返回抵达。
    // 文件传输过滤器插在消费器与 xterm 之间（仅 UTF-8：GBK 转码会毁二进制协议帧）
    const isUtf8 =
      target.kind === 'spec'
        ? !target.spec.encoding || target.spec.encoding === 'utf-8'
        : !target.encoding || target.encoding === 'utf-8';
    this.transfer = isUtf8
      ? await createTransferFilter({
          term,
          writeBytes: (b) => this.writeBytes(b),
          notify: this.notify,
        })
      : null;
    // 新数据通道 = 新代际：epoch 建链时生成并随 term_open 上报后端（PR-6）。
    // credit 闭包捕获创建时点 epoch（禁止动态读取当前流身份）；tabId 动态读取由
    // 后端 epoch 校验兜底——旧 callback 迟到 ACK 携带旧 epoch 被无条件丢弃。
    const streamEpoch = newStreamEpoch();
    this.consumer = new StreamConsumer(
      streamEpoch,
      (chunk, cb) => (this.transfer ? this.transfer.onOutput(chunk, cb) : term.write(chunk, cb)),
      (ackedTotal) => {
        if (this.tabId) void invoke('term_credit', { tabId: this.tabId, streamEpoch, ackedTotal });
      },
    );
    const data = createStreamChannel((frame) => this.consumer?.push(frame));
    const events = new Channel<TermEvent>();
    events.onmessage = (ev) => {
      this.onEvent(ev);
      if (ev.type === 'session_state') this.stateHook?.(ev);
    };
    // 输入订阅必须先于 term_open 注册：连接建立可能耗时数秒（WAN 首连 + hostkey
    // 确认），期间键入若无订阅会被 xterm 直接丢弃（竞态：键入丢失）。
    // tabId 未赋值时 write 入队缓冲，开链后按序补发；广播钩子仅在开链后触发。
    // 传输期间输入交协议处理（^C 中断 trzsz；ZMODEM 期间丢弃防污染）
    this.disposers.push(
      term.onData((s) => {
        if (this.transfer?.transferring) {
          this.transfer.processInput(s);
          return;
        }
        this.write(s);
        if (this.tabId) this.inputHook?.(s);
      }),
      term.onBinary((s) => {
        if (this.transfer?.transferring) this.transfer.processBinary(s);
      }),
    );

    // term_open 可能耗时数秒（WAN 首连 + hostkey 确认），期间 fit 可能改尺寸；
    // 记下开链尺寸，连接建立后比对补发 resize（onResize 注册前的变更会丢）
    const openedCols = term.cols;
    const openedRows = term.rows;
    const res = await invoke<{ tabId: string }>('term_open', {
      spec: target.kind === 'spec' ? target.spec : null,
      sessionId: target.kind === 'session' ? target.sessionId : null,
      // 终端编码：档案会话取建档快照（缺省由后端读档案），内联 spec 取其自身字段
      encoding: target.kind === 'spec' ? (target.spec.encoding ?? null) : (target.encoding ?? null),
      // 数据通道代际标识：本 tab 信用 ACK 身份的一部分
      streamEpoch,
      data,
      events,
      cols: openedCols,
      rows: openedRows,
    });
    this.tabId = res.tabId;
    // 开链前若已知在后台（隐藏 tab 挂载），补同步 focus 标记
    if (!this.focused) void invoke('term_focus', { tabId: res.tabId, focused: false });
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

    // resize 订阅在开链后注册：开链前的尺寸变更由上方 cols/rows 比对补发，
    // 提前注册也只会因 tabId 未赋值而空转
    this.disposers.push(
      term.onResize(({ cols, rows }) => {
        if (this.tabId) void invoke('term_resize', { tabId: this.tabId, cols, rows });
      }),
    );
    return this.tabId;
  }

  /** 前台/后台切换（PR-17 二期）：后台信用耗尽转 ring buffer，回前台回放 */
  setFocused(focused: boolean): void {
    this.focused = focused;
    if (this.tabId) void invoke('term_focus', { tabId: this.tabId, focused });
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

  /** 协议二进制写入（ZMODEM/trzsz 帧）：b64 → term_input_raw，不经输入编码器 */
  writeBytes(bytes: Uint8Array): void {
    if (!this.tabId) return;
    void invoke('term_input_raw', {
      tabId: this.tabId,
      b64: bytesToB64(bytes),
    });
  }

  /** 释放输入/resize 订阅与消费器（OSC 7 handler 重注册即覆盖，无需释放） */
  private detachInput(): void {
    for (const d of this.disposers) d.dispose();
    this.disposers = [];
    this.transfer?.dispose();
    this.transfer = null;
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
