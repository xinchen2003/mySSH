import { beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: unknown = null;
  },
}));

import { invoke } from '@tauri-apps/api/core';
import type { Terminal } from '@xterm/xterm';
import { TerminalSession } from './terminal-session';
import type { ConnectTarget } from './types';

/** 最小 xterm 替身：捕获 onData 回调，write 立即回调（不渲染） */
function fakeTerm(cols = 80, rows = 24) {
  const cbs = {
    data: (_s: string): void => {
      throw new Error('onData 未注册');
    },
  };
  const term = {
    cols,
    rows,
    onData: (cb: (s: string) => void) => {
      cbs.data = cb;
      return { dispose: () => undefined };
    },
    onBinary: () => ({ dispose: () => undefined }),
    onResize: () => ({ dispose: () => undefined }),
    parser: { registerOscHandler: () => true },
    write: (_c: Uint8Array, cb?: () => void) => cb?.(),
  } as unknown as Terminal;
  return { term, cbs };
}

// GBK 会话不挂传输过滤器，测试聚焦输入缓冲路径
const target: ConnectTarget = {
  kind: 'spec',
  spec: {
    host: 'h',
    port: 22,
    user: 'u',
    auth: { type: 'password', password: 'p' },
    encoding: 'gbk',
  },
};

/** 收集 term_input 调用并解码回文本 */
function sentInputs(): string[] {
  return vi
    .mocked(invoke)
    .mock.calls.filter(([c]) => c === 'term_input')
    .map(([, a]) => new TextDecoder().decode(new Uint8Array((a as { bytes: number[] }).bytes)));
}

describe('TerminalSession 连接建立期输入缓冲', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    // StreamConsumer 的 rAF 对齐循环在 node 环境无此 API；桩住即可（不 push 帧）
    vi.stubGlobal('requestAnimationFrame', () => 0);
  });

  it('term_open 未返回期间的键入被缓存，开链后按序补发', async () => {
    let resolveOpen!: (v: { tabId: string }) => void;
    vi.mocked(invoke).mockImplementation((cmd) =>
      cmd === 'term_open' ? new Promise((r) => (resolveOpen = r)) : Promise.resolve(undefined),
    );

    const { term, cbs } = fakeTerm();
    const session = new TerminalSession(() => undefined);
    const attaching = session.attach(term, target);

    // 连接建立期键入：不得立即下发，也不得丢弃
    cbs.data('ls');
    cbs.data('\n');
    expect(sentInputs()).toEqual([]);

    resolveOpen({ tabId: 't1' });
    expect(await attaching).toBe('t1');
    expect(sentInputs()).toEqual(['ls', '\n']);

    // 开链后键入直发
    cbs.data('pwd\n');
    expect(sentInputs()).toEqual(['ls', '\n', 'pwd\n']);
  });

  it('开链前的输入不触发广播钩子（未开链不扇出）', async () => {
    let resolveOpen!: (v: { tabId: string }) => void;
    vi.mocked(invoke).mockImplementation((cmd) =>
      cmd === 'term_open' ? new Promise((r) => (resolveOpen = r)) : Promise.resolve(undefined),
    );

    const { term, cbs } = fakeTerm();
    const session = new TerminalSession(() => undefined);
    const hook = vi.fn();
    session.inputHook = hook;
    const attaching = session.attach(term, target);

    cbs.data('early\n');
    expect(hook).not.toHaveBeenCalled();

    resolveOpen({ tabId: 't2' });
    await attaching;
    cbs.data('late\n');
    expect(hook).toHaveBeenCalledTimes(1);
    expect(hook).toHaveBeenCalledWith('late\n');
  });
});
