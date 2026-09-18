/**
 * 终端内文件传输（规格书 M3「ZMODEM（rz/sz）兼容」+ trzsz）。
 *
 * 挂载在输出链 xterm 之前：ZMODEM 哨兵消费协议帧 → trzsz 过滤器 → xterm。
 * 协议帧绝不进 xterm；下行信用（term_credit）在帧被协议吃掉时立即回传，
 * 空闲路径保持「最后一写携带 done」的既有 rAF 背压语义。
 *
 * 已知限制（v1，.scratch/zmodem-trzsz/spec.md）：
 * - 仅 UTF-8 会话（GBK 输出转码会毁二进制协议帧）；GBK 会话不挂过滤器。
 * - ZMODEM 收方向整文件驻内存（spool + b64）再落盘；大文件走 SFTP。
 * - trzsz 的交互式选择依赖 WebView 的 File System Access API；不可用时
 *   拖拽上传路径（uploadFiles）仍可用，交互式 trz/tsz 会报不支持。
 */
import { TrzszFilter } from 'trzsz';
import { save } from '@tauri-apps/plugin-dialog';
import { invoke } from '@tauri-apps/api/core';
import type { Terminal } from '@xterm/xterm';
import { tNow } from '../i18n';
import type { NotificationLevel } from '../state/app-store';

/* zmodem.js dist 包模块级访问 window：懒加载（浏览器/WebView 才有 DOM），
 * 避免 node 测试环境（i18n→app-store 链路）拉起即炸 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
let Zmodem: any = null;
let zmodemLoading: Promise<void> | null = null;
function ensureZmodem(): Promise<void> {
  zmodemLoading ??= import('zmodem.js/dist/zmodem.js').then(() => {
    Zmodem = (globalThis as { Zmodem?: unknown }).Zmodem;
    if (!Zmodem) throw new Error('zmodem.js 初始化失败（无全局 Zmodem）');
  });
  return zmodemLoading;
}

const enc = new TextEncoder();

/** zmodem.js 会话面（无官方类型；仅用到的方法面） */
interface ZSession {
  type?: string;
  on(ev: string, cb: (arg: unknown) => void): void;
  /** 收方向必须显式 start()：发送 ZRINIT 并武装 offer 等待；缺它 sz 死等 */
  start?(): unknown;
  close(): void;
  abort?(): void;
}

/** zmodem.js 收方向的文件 offer */
interface ZmodemOffer {
  get_details(): { name: string; size: number };
  accept(): Promise<Uint8Array[]>;
}

export interface TransferFilter {
  /** 输出链入口：协议帧被消费时立即回 done（信用）；干净字节走 xterm 背压 */
  onOutput(chunk: Uint8Array, done: () => void): void;
  /** 传输期间的用户输入（^C 中断等）；空闲态不走这里 */
  processInput(s: string): void;
  processBinary(s: string): void;
  readonly transferring: boolean;
  dispose(): void;
}

/** trzsz 触发行的两种字面头（tsz/trz 带 \x1b7\x07 前缀；防御性兼容裸头） */
const TRIG_HEADS = ['\x1b7\x07::TRZSZ:TRANSFER:', '::TRZSZ:TRANSFER:'];
const TRIG_TAIL = /^[SRD]?((:|\.)[0-9]*)*\r?$/;
/** 暂存上限：触发行实测 ~50 字节，超限即非触发，放行 */
const TRIG_MAX_HOLD = 128;

/**
 * trzsz 触发串跨块保险（真机验收发现）：
 * TrzszFilter 的 detectAndHandleTrzsz 只在**单次喂入**的字节里一次性匹配
 * `::TRZSZ:TRANSFER:…`，无跨块缓冲；WAN 上触发行被 TCP/聚合边界切开后，
 * 传输永远不会启动（触发行原文直接上屏）。
 *
 * 规则：最后一个换行之后的行尾片段若是触发行的前缀（或触发行未完成形态），
 * 暂存不转发，下一块拼回；否则立即放行——普通无换行行尾（如 shell 提示符）
 * 不得滞留。flush 用于会话释放时兜底放行。
 */
export function createTrzszTriggerGuard(forward: (chunk: Uint8Array) => void): {
  push(chunk: Uint8Array): void;
  flush(): void;
} {
  let pending: Uint8Array | null = null;
  const dec = new TextDecoder('latin1');

  /** 行尾是否为触发前缀/未完成触发行 */
  function isTriggerPartial(tail: Uint8Array): boolean {
    if (tail.length === 0 || tail.length > TRIG_MAX_HOLD) return false;
    const s = dec.decode(tail);
    for (const head of TRIG_HEADS) {
      if (head.startsWith(s)) return true; // 头是 tail 的前缀延展（tail 尚未到头长）
      if (s.startsWith(head) && TRIG_TAIL.test(s.slice(head.length))) return true;
    }
    return false;
  }

  return {
    push(chunk) {
      let data = chunk;
      if (pending) {
        const merged = new Uint8Array(pending.length + chunk.length);
        merged.set(pending);
        merged.set(chunk, pending.length);
        pending = null;
        data = merged;
      }
      let lastNl = -1;
      for (let i = data.length - 1; i >= 0; i--) {
        if (data[i] === 0x0a) {
          lastNl = i;
          break;
        }
      }
      const tail = data.subarray(lastNl + 1);
      if (isTriggerPartial(tail)) {
        const keep = tail.slice(); // 拷贝：避免小尾巴挂住大块聚合缓冲
        if (tail.length < data.length) forward(data.subarray(0, data.length - tail.length));
        pending = keep;
        return;
      }
      forward(data);
    },
    flush() {
      if (pending) {
        forward(pending);
        pending = null;
      }
    },
  };
}
export async function createTransferFilter(opts: {
  term: Terminal;
  /** 协议输出（二进制）→ term_input_raw，不经输入编码器 */
  writeBytes: (b: Uint8Array) => void;
  notify: (msg: string, level?: NotificationLevel) => void;
}): Promise<TransferFilter> {
  await ensureZmodem();
  const { term, writeBytes, notify } = opts;
  // 本帧经解析后应写到终端的净荷；to_terminal 链同步回调收集
  let pieces: Uint8Array[] = [];
  /** 本帧刚确认了新 ZMODEM 会话：zmodem.js 会把初始 ZRQINIT/ZRINIT 头
      原样透传到终端（其官方注释称“本来就该上屏”），需手动剥掉尾部帧头 */
  let justDetected = false;
  // ZMODEM 会话活动标记；trzsz 活动由 filter 自查
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  let zsession: any = null;

  // BufferType → Uint8Array（Blob 仅 Node 路径出现，浏览器模式恒 string|Uint8Array；类型防御）
  const toBytes = (d: ArrayBuffer | Uint8Array | Blob): Uint8Array => {
    if (d instanceof Blob) {
      void d.arrayBuffer().then((ab) => writeBytes(new Uint8Array(ab)));
      return new Uint8Array(0);
    }
    return d instanceof Uint8Array ? d : new Uint8Array(d);
  };
  const trzsz = new TrzszFilter({
    writeToTerminal: (d) => pieces.push(typeof d === 'string' ? enc.encode(d) : toBytes(d)),
    sendToServer: (d) => writeBytes(typeof d === 'string' ? enc.encode(d) : toBytes(d)),
    terminalColumns: term.cols,
  });
  term.onResize(({ cols }) => trzsz.setTerminalColumns(cols));

  // zmodem.js 的 to_terminal 吐出的是裸 number[]（非 Uint8Array）——trzsz 的检测器
  // 只认 string/ArrayBuffer/Uint8Array，裸数组会被静默跳过（真机验收发现的根因）；
  // 守卫入口归一化，同时提供触发串跨块保险（见 createTrzszTriggerGuard）
  const trzGuard = createTrzszTriggerGuard((b) => trzsz.processServerOutput(b));
  const sentry = new Zmodem.Sentry({
    to_terminal: (bytes: number[] | Uint8Array) =>
      trzGuard.push(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes)),
    sender: (bytes: number[] | Uint8Array) => writeBytes(new Uint8Array(bytes)),
    on_detect: handleDetect,
    on_retract: () => {
      /* 检测被撤回（后续字节证明非 ZMODEM）：无需动作，链路已自行续流 */
    },
  });

  function handleDetect(detection: { confirm(): unknown }): void {
    let zs: ZSession;
    try {
      zs = detection.confirm() as ZSession;
    } catch {
      return;
    }
    zsession = zs;
    zs.on('session_end', () => {
      zsession = null;
    });
    justDetected = true;
    if (zs.type === 'receive') {
      // 远端 sz：收文件，逐 offer 接收 → 保存对话框 → transfer_save_file。
      // 必须 start()：sz 发 ZRQINIT 后死等接收方 ZRINIT，不发则永远等不到 offer
      notify(tNow('state.zmodemIncoming'), 'info');
      zs.on('offer', (o) => void receiveOffer(o as ZmodemOffer));
      zs.start?.();
    } else {
      // 远端 rz：弹文件选择 → Browser.send_files 驱动发送
      void sendFlow(zs);
    }
  }

  async function receiveOffer(offer: ZmodemOffer): Promise<void> {
    const det = offer.get_details();
    try {
      const spool = await offer.accept();
      const path = await save({ defaultPath: det.name });
      if (!path) return; // 已 accept 无法撤回，丢弃内容（与 Xshell 取消保存一致）
      const total = spool.reduce((n, p) => n + p.length, 0);
      const merged = new Uint8Array(total);
      let off = 0;
      for (const p of spool) {
        merged.set(p, off);
        off += p.length;
      }
      await invoke('transfer_save_file', { path, b64: bytesToB64(merged) });
      notify(tNow('state.zmodemSaved', { name: det.name }), 'success');
    } catch (e) {
      notify(tNow('state.zmodemSaveFailed', { error: String(e) }), 'error');
    }
  }

  async function sendFlow(zs: ZSession): Promise<void> {
    const files = await pickFiles();
    if (!files || files.length === 0) {
      try {
        zs.close();
      } catch {
        /* 会话已死 */
      }
      zsession = null;
      return;
    }
    try {
      await Zmodem.Browser.send_files(zs, files, {
        on_file_complete: (f: { name: string }) =>
          notify(tNow('state.zmodemSent', { name: f.name }), 'success'),
      });
      zs.close();
    } catch (e) {
      notify(tNow('state.zmodemSendFailed', { error: String(e) }), 'error');
      try {
        zs.abort?.();
      } catch {
        /* 尽力而为 */
      }
      zsession = null;
    }
  }

  return {
    onOutput(chunk, done) {
      pieces = [];
      try {
        sentry.consume(chunk);
      } catch (e) {
        // 协议异常：原帧直写终端（可见乱码优于静默丢数据）并告警
        notify(tNow('state.zmodemError', { error: String(e) }), 'error');
        pieces.push(chunk);
      }
      if (pieces.length === 0) {
        // 帧被协议整体消费（或缓冲于 trzsz）：立即回传信用
        done();
        return;
      }
      if (justDetected) {
        // 确认会话的那帧会夹带初始帧头上屏（zmodem.js 设计如此）——剥掉
        justDetected = false;
        pieces = stripTrailingZmodemHeader(pieces);
        if (pieces.length === 0) {
          done();
          return;
        }
      }
      if (zsession !== null || trzsz.isTransferringFiles()) {
        // 传输期间 trzsz 进度条等净荷：直写终端，不经 xterm 写回调链
        for (const p of pieces) term.write(p);
        done();
        return;
      }
      // 空闲：维持既有背压语义——最后一个写携带 done
      for (let i = 0; i < pieces.length - 1; i++) term.write(pieces[i]);
      term.write(pieces[pieces.length - 1], done);
    },
    processInput(s) {
      trzsz.processTerminalInput(s);
    },
    processBinary(s) {
      trzsz.processBinaryInput(s);
    },
    get transferring() {
      return zsession !== null || trzsz.isTransferringFiles();
    },
    dispose() {
      // 暂存的触发行残片放行，避免会话关闭吞掉尾部输出
      trzGuard.flush();
    },
  };
}

/** Uint8Array → base64（分块避免 String.fromCharCode 栈溢出） */
export function bytesToB64(bytes: Uint8Array): string {
  let bin = '';
  const step = 0x8000;
  for (let i = 0; i < bytes.length; i += step) {
    bin += String.fromCharCode(...bytes.subarray(i, i + step));
  }
  return btoa(bin);
}

/** 剥掉净荷尾部的 ZMODEM 初始帧头（**<CAN>B + 14 hex + CR [0x8a] [XON]）。
 * 哨兵在确认会话的那帧里把头字节一并透传；对上屏无意义且形如乱码。 */
export function stripTrailingZmodemHeader(pieces: Uint8Array[]): Uint8Array[] {
  const total = pieces.reduce((n, p) => n + p.length, 0);
  const joined = new Uint8Array(total);
  let off = 0;
  for (const p of pieces) {
    joined.set(p, off);
    off += p.length;
  }
  // 匹配尾部帧头：2A 2A 18 42 + 14 个 hex ASCII + 0D (+8A) (+11)
  let i = joined.length - 1;
  if (i >= 0 && joined[i] === 0x11) i--; // XON
  if (i >= 0 && joined[i] === 0x8a) i--;
  if (i < 0 || joined[i] !== 0x0d) return pieces; // CR
  const hexEnd = i; // 14 个 hex 在 CR 之前
  const hexStart = hexEnd - 14;
  if (hexStart < 3) return pieces;
  for (let j = hexStart; j < hexEnd; j++) {
    const c = joined[j];
    const isHex = (c >= 0x30 && c <= 0x39) || (c >= 0x61 && c <= 0x66) || (c >= 0x41 && c <= 0x46);
    if (!isHex) return pieces;
  }
  if (
    joined[hexStart - 1] !== 0x42 || // 'B'
    joined[hexStart - 2] !== 0x18 || // CAN
    joined[hexStart - 3] !== 0x2a || // '*'
    joined[hexStart - 4] !== 0x2a // '*'
  ) {
    return pieces;
  }
  const cut = joined.subarray(0, hexStart - 4);
  return cut.length ? [cut] : [];
}

/** 原生文件选择（WebView2 支持 <input type=file>；不依赖 FS Access API） */
function pickFiles(): Promise<File[] | null> {
  return new Promise((resolve) => {
    const input = document.createElement('input');
    input.type = 'file';
    input.multiple = true;
    input.style.display = 'none';
    const finish = (files: File[] | null) => {
      window.clearTimeout(timer);
      input.remove();
      resolve(files);
    };
    // Chromium 113+：取消也触发 cancel 事件
    input.oncancel = () => finish(null);
    input.onchange = () => finish(Array.from(input.files ?? []));
    // 兜底：对话框遗忘了 cancel 事件的场合，超时回收
    const timer = window.setTimeout(() => finish(null), 10 * 60 * 1000);
    document.body.appendChild(input);
    input.click();
  });
}
