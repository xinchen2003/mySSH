import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: unknown = null;
  },
}));
vi.mock('@tauri-apps/api/event', () => ({
  emit: vi.fn(() => Promise.resolve()),
  listen: vi.fn(() => Promise.resolve(() => undefined)),
}));

import { invoke } from '@tauri-apps/api/core';
import { emit, listen } from '@tauri-apps/api/event';
import { BROADCAST_INPUT_EVENT, initBroadcastReceiver } from '../term/broadcast';
import { useAppStore } from './app-store';
import type { SessionRecord, TermOpenSpec } from '../term/types';

const spec: TermOpenSpec = {
  host: 'h',
  port: 22,
  user: 'u',
  auth: { type: 'agent' },
};

const rec: SessionRecord = {
  id: 's1',
  name: 'web-01',
  host: 'example.com',
  port: 22,
  username: 'root',
  authType: 'password',
  jumpChain: [],
  groupPath: '生产/Web',
  tags: [],
  createdAt: '2026-01-01',
  updatedAt: '2026-01-01',
};

const state = () => useAppStore.getState();

beforeEach(() => {
  useAppStore.setState({
    tabs: [],
    activeId: null,
    notices: [],
    pendingCloseTabs: null,
    pendingDeleteSession: null,
    settings: {},
    sessions: [],
    broadcastEnabled: false,
  });
  vi.mocked(invoke).mockReset();
});

describe('app-store tabs', () => {
  it('moveTab 把拖拽标签移到目标之前，顺序其余不变', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    const ids = state().tabs.map((t) => t.id);
    expect(ids).toHaveLength(3);

    // 第三个移到第一个之前
    s.moveTab(ids[2], ids[0]);
    const after = state().tabs.map((t) => t.id);
    expect(after).toEqual([ids[2], ids[0], ids[1]]);

    // 自移/未知 id 无副作用
    s.moveTab(ids[2], ids[2]);
    expect(state().tabs.map((t) => t.id)).toEqual(after);
    s.moveTab('nope', ids[0]);
    expect(state().tabs.map((t) => t.id)).toEqual(after);
  });
});

describe('关标签确认守卫（7.6）', () => {
  it('默认开启：有活跃连接时 closeTab 不直接关闭，而是进入待确认', () => {
    const s = state();
    s.connect(spec); // 新 pane 初始状态 connecting = 活跃
    const id = state().tabs[0].id;

    s.closeTab(id);
    expect(state().tabs).toHaveLength(1);
    expect(state().pendingCloseTabs).toEqual([id]);

    state().confirmCloseTab();
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('取消确认后标签保留', () => {
    const s = state();
    s.connect(spec);
    const id = state().tabs[0].id;

    s.closeTab(id);
    state().cancelCloseTab();
    expect(state().tabs).toHaveLength(1);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('设置关闭（terminal.confirmCloseTab=false）时直接关闭', () => {
    useAppStore.setState({ settings: { 'terminal.confirmCloseTab': false } });
    const s = state();
    s.connect(spec);
    s.closeTab(state().tabs[0].id);
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('全部 pane 已关闭时直接关闭（不弹确认）', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    s.setPaneState(tab.id, tab.activePaneId, 'closed');

    s.closeTab(tab.id);
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('closePane 关闭最后一叶也走确认守卫', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];

    s.closePane(tab.id, tab.activePaneId);
    expect(state().tabs).toHaveLength(1); // 守卫拦截，标签未关
    expect(state().pendingCloseTabs).toEqual([tab.id]);

    state().confirmCloseTab();
    expect(state().tabs).toHaveLength(0);
  });
});

describe('closePane 分屏关闭（7.3）', () => {
  it('关闭非末叶 pane：标签保留，活跃 pane 切换', () => {
    const s = state();
    s.connect(spec);
    s.splitActive('row');
    const tab = state().tabs[0];
    const [first, second] = Object.keys(tab.panes);
    expect(tab.activePaneId).toBe(second); // 分屏后新 pane 为活跃

    s.closePane(tab.id, second);
    const after = state().tabs[0];
    expect(Object.keys(after.panes)).toEqual([first]);
    expect(after.activePaneId).toBe(first);
    expect(state().pendingCloseTabs).toBeNull(); // 非末叶不触发关标签确认
  });
});

describe('多标签关闭（批次四 10.3）', () => {
  it('closeOtherTabs / closeTabsToRight / closeAllTabs 各自选中正确的标签集合', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    // 全部 pane 置为已关闭 → 不触发确认，直接关闭
    for (const t of state().tabs) s.setPaneState(t.id, t.activePaneId, 'closed');
    const [a, b] = state().tabs.map((t) => t.id);

    s.closeTabsToRight(b);
    expect(state().tabs.map((t) => t.id)).toEqual([a, b]);

    s.closeOtherTabs(a);
    expect(state().tabs.map((t) => t.id)).toEqual([a]);

    s.connect(spec);
    s.setPaneState(state().tabs[1].id, state().tabs[1].activePaneId, 'closed');
    s.closeAllTabs();
    expect(state().tabs).toHaveLength(0);
  });

  it('批量关闭含活跃连接：汇总一次确认，确认后全部关闭', () => {
    const s = state();
    s.connect(spec); // connecting = 活跃
    s.connect(spec);
    const closedTab = () => {
      s.connect(spec);
      const t = state().tabs[2];
      s.setPaneState(t.id, t.activePaneId, 'closed');
    };
    closedTab();

    s.closeAllTabs();
    expect(state().tabs).toHaveLength(3); // 守卫拦截
    expect(state().pendingCloseTabs).toHaveLength(3);

    state().confirmCloseTab();
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTabs).toBeNull();
  });

  it('批量关闭取消后一个都不关', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    const ids = state().tabs.map((t) => t.id);

    s.closeAllTabs();
    state().cancelCloseTab();
    expect(state().tabs.map((t) => t.id)).toEqual(ids);
  });
});

describe('删除服务器确认（7.1）', () => {
  it('request/cancel 管理待确认状态', () => {
    state().requestDeleteSession(rec);
    expect(state().pendingDeleteSession?.id).toBe('s1');
    state().cancelDeleteSession();
    expect(state().pendingDeleteSession).toBeNull();
  });

  it('确认删除成功 → success 通知', async () => {
    vi.mocked(invoke).mockResolvedValue([]);
    state().requestDeleteSession(rec);
    await state().confirmDeleteSession();

    expect(invoke).toHaveBeenCalledWith('session_delete', { sessionId: 's1' });
    expect(state().pendingDeleteSession).toBeNull();
    const n = state().notices.at(-1);
    expect(n?.level).toBe('success');
    expect(n?.message).toContain('web-01');
  });

  it('确认删除失败 → error 通知（与成功不同级别）', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('db locked'));
    state().requestDeleteSession(rec);
    await state().confirmDeleteSession();

    const n = state().notices.at(-1);
    expect(n?.level).toBe('error');
    expect(n?.message).toContain('删除服务器失败');
  });
});

describe('通知分级生命周期（7.7）', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.clearAllTimers();
    vi.useRealTimers();
  });

  it('默认 info 级，4s 后自动消失', () => {
    state().notify('hello');
    expect(state().notices).toHaveLength(1);
    expect(state().notices[0].level).toBe('info');

    vi.advanceTimersByTime(3999);
    expect(state().notices).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(state().notices).toHaveLength(0);
  });

  it('success 3s 消失；warning 8s 消失', () => {
    state().notify('ok', 'success');
    state().notify('warn', 'warning');
    vi.advanceTimersByTime(3000);
    expect(state().notices.map((n) => n.level)).toEqual(['warning']);
    vi.advanceTimersByTime(5000);
    expect(state().notices).toHaveLength(0);
  });

  it('error 常驻不自动消失，dismissNotice 手动关闭', () => {
    state().notify('boom', 'error');
    vi.advanceTimersByTime(60_000);
    expect(state().notices).toHaveLength(1);

    state().dismissNotice(state().notices[0].id);
    expect(state().notices).toHaveLength(0);
  });

  it('堆叠上限 5：溢出优先丢弃最旧的非 error', () => {
    state().notify('e1', 'error');
    for (let i = 0; i < 6; i++) state().notify(`i${i}`, 'info');
    const levels = state().notices.map((n) => n.level);
    expect(state().notices).toHaveLength(5);
    expect(levels[0]).toBe('error'); // error 不被挤出
    expect(state().notices[1].message).toBe('i2'); // 最旧的 info 先丢
  });
});
describe('批次六：通知动作 / bell / disconnectPane / 传输计数', () => {
  it('notify 可携带可序列化动作（不含函数引用）', () => {
    state().notify('已导出: /tmp/x.json', 'success', {
      label: '打开所在目录',
      actionId: 'open-in-explorer',
      arg: '/tmp/x.json',
    });
    const n = state().notices[0];
    expect(n.action).toEqual({
      label: '打开所在目录',
      actionId: 'open-in-explorer',
      arg: '/tmp/x.json',
    });
    // 可序列化契约：结构化克隆不丢字段
    expect(JSON.parse(JSON.stringify(n))).toEqual(n);
  });

  it('markBell 去重；setActive 清除该标签标记；关标签顺带清理', () => {
    const s = state();
    useAppStore.setState({ settings: { 'terminal.confirmCloseTab': false } });
    s.connect(spec);
    s.connect(spec);
    const [t1, t2] = state().tabs.map((t) => t.id);

    s.markBell(t2);
    s.markBell(t2);
    expect(state().bellTabs).toEqual([t2]);

    s.setActive(t2);
    expect(state().bellTabs).toEqual([]);

    s.markBell(t1);
    state().requestCloseTabs([t1]);
    expect(state().bellTabs).toEqual([]);
  });

  it('disconnectPane 只断活跃 pane 并置 closed；closed pane 幂等', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    const pid = tab.activePaneId;
    s.setPaneState(tab.id, pid, 'connected');

    s.disconnectPane(tab.id, pid);
    expect(state().tabs[0].panes[pid].state).toBe('closed');
    // 已 closed 再调用不抛不错位
    s.disconnectPane(tab.id, pid);
    expect(state().tabs[0].panes[pid].state).toBe('closed');
  });

  it('setTransferActive 设置与清空', () => {
    expect(state().transferActive).toBeNull();
    state().setTransferActive(3);
    expect(state().transferActive).toBe(3);
    state().setTransferActive(null);
    expect(state().transferActive).toBeNull();
  });
});
describe('分屏保活与同服务器新 pane（UX-6）', () => {
  it('splitActive 不关闭旧 pane 会话，旧 pane 保留且新 pane 复用同一 target', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    const oldPane = tab.panes[tab.activePaneId];
    const closeSpy = vi.spyOn(oldPane.session, 'close');

    s.splitActive('row');
    const after = state().tabs[0];
    expect(closeSpy).not.toHaveBeenCalled();
    expect(after.panes[oldPane.id]).toBeDefined(); // 旧 pane 数据仍在
    expect(after.target).toEqual({ kind: 'spec', spec }); // 新 pane 经 tab.target 连同服务器
    const newPane = after.panes[after.activePaneId];
    expect(newPane.id).not.toBe(oldPane.id);
  });

  it('源 pane cwd 已知时新 pane 携带初始目录；cwd 未知则不携带', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    tab.panes[tab.activePaneId].session.cwd = '/var/log';
    s.splitActive('row');
    expect(state().tabs[0].panes[state().tabs[0].activePaneId].initialCwd).toBe('/var/log');

    // 第二个分屏的源 pane 无 cwd → 不带初始目录
    const t2 = state().tabs[0];
    t2.panes[t2.activePaneId].session.cwd = null;
    s.splitActive('col');
    const t3 = state().tabs[0];
    expect(t3.panes[t3.activePaneId].initialCwd).toBeUndefined();
  });

  it('首连成功（非重连）后向新 pane 写入一次 cd 并消费 initialCwd', () => {
    const s = state();
    s.connect(spec);
    const t0 = state().tabs[0];
    t0.panes[t0.activePaneId].session.cwd = "/opt/a'b"; // 含单引号需转义
    s.splitActive('row');
    const tab = state().tabs[0];
    const p = tab.panes[tab.activePaneId];
    const writeSpy = vi.spyOn(p.session, 'write').mockImplementation(() => undefined);

    // 注入序列：先 stty -echo 关回显（唯一可见行），cd 同步写入（无回显更干净），
    // 600ms 后写 OSC 7 钩子并恢复回显（钩子整段不上屏）
    vi.useFakeTimers(); // 须在 onEvent 前挂钟，注入定时器才受控
    // 模拟后端 connected 帧（等价事件通道回调；onEvent 公开作测试接缝）
    p.session.onEvent({ v: 1, type: 'session_state', tabId: 'x', state: 'connected' });
    expect(writeSpy).toHaveBeenCalledTimes(2);
    expect(writeSpy.mock.calls[0][0]).toBe('stty -echo\n');
    expect(writeSpy.mock.calls[1][0]).toBe("cd '/opt/a'\\''b'\n");
    vi.advanceTimersByTime(700);
    vi.useRealTimers();
    expect(writeSpy).toHaveBeenCalledTimes(3);
    expect(writeSpy.mock.calls[2][0]).toContain('_myssh_osc7');
    expect(writeSpy.mock.calls[2][0]).toContain('stty echo');
    expect(state().tabs[0].panes[p.id].initialCwd).toBeUndefined(); // 已消费
  });

  it('断线重连成功（reconnected=true）不触发初始 cd', async () => {
    const s = state();
    s.connect(spec);
    const t0 = state().tabs[0];
    t0.panes[t0.activePaneId].session.cwd = '/var/log';
    s.splitActive('row');
    const tab = state().tabs[0];
    const p = tab.panes[tab.activePaneId];
    const writeSpy = vi.spyOn(p.session, 'write').mockImplementation(() => undefined);

    vi.useFakeTimers(); // 须在 onEvent 前挂钟，注入定时器才受控
    p.session.onEvent({
      v: 1,
      type: 'session_state',
      tabId: 'x',
      state: 'connected',
      reconnected: true,
    });
    // 重连 = 新 shell 进程，OSC 7 钩子需重注（stty -echo → 600ms 后钩子）；初始 cd 不触发
    expect(writeSpy).toHaveBeenCalledTimes(1);
    expect(writeSpy.mock.calls[0][0]).toBe('stty -echo\n');
    vi.advanceTimersByTime(700);
    vi.useRealTimers();
    expect(writeSpy).toHaveBeenCalledTimes(2);
    expect(writeSpy.mock.calls[1][0]).toContain('_myssh_osc7');
    expect(writeSpy.mock.calls[1][0]).not.toContain('cd ');
    expect(state().tabs[0].panes[p.id].initialCwd).toBe('/var/log'); // 留给真正的首连
  });
});

describe('广播输入（UX-11）', () => {
  it('开启后扇出到同窗口其它 connected pane；发起 pane 与未连接 pane 不写；关闭后恢复', () => {
    const s = state();
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    const [t1, t2, t3] = state().tabs;
    const p1 = t1.panes[t1.activePaneId];
    const p2 = t2.panes[t2.activePaneId];
    const p3 = t3.panes[t3.activePaneId];
    s.setPaneState(t1.id, p1.id, 'connected');
    s.setPaneState(t2.id, p2.id, 'connected');
    s.setPaneState(t3.id, p3.id, 'closed'); // 未连接不收广播
    const w1 = vi.spyOn(p1.session, 'write').mockImplementation(() => undefined);
    const w2 = vi.spyOn(p2.session, 'write').mockImplementation(() => undefined);
    const w3 = vi.spyOn(p3.session, 'write').mockImplementation(() => undefined);
    vi.mocked(emit).mockClear();

    s.toggleBroadcast();
    expect(state().broadcastEnabled).toBe(true);
    p1.session.inputHook?.('ls\n');
    expect(w1).not.toHaveBeenCalled(); // 发起 pane 由自身 onData 直发，不经广播
    expect(w2).toHaveBeenCalledWith('ls\n');
    expect(w3).not.toHaveBeenCalled();
    // 跨窗口事件帧带本窗口 label 作回环防护
    expect(vi.mocked(emit)).toHaveBeenCalledWith(BROADCAST_INPUT_EVENT, {
      v: 1,
      source: 'main',
      data: 'ls\n',
    });

    // 关闭开关即恢复单 pane 输入
    s.toggleBroadcast();
    w2.mockClear();
    vi.mocked(emit).mockClear();
    p1.session.inputHook?.('pwd\n');
    expect(w2).not.toHaveBeenCalled();
    expect(vi.mocked(emit)).not.toHaveBeenCalled();
  });

  it('跨窗口接收：写入本窗口 connected pane；本窗口来源帧被回环防护丢弃', async () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    const p = tab.panes[tab.activePaneId];
    s.setPaneState(tab.id, p.id, 'connected');
    const w = vi.spyOn(p.session, 'write').mockImplementation(() => undefined);

    await initBroadcastReceiver();
    const handler = vi.mocked(listen).mock.calls[0][1] as (e: {
      payload: { v: 1; source: string; data: string };
    }) => void;
    expect(vi.mocked(listen).mock.calls[0][0]).toBe(BROADCAST_INPUT_EVENT);

    handler({ payload: { v: 1, source: 'det-s1', data: 'top\n' } });
    expect(w).toHaveBeenCalledWith('top\n');

    w.mockClear();
    handler({ payload: { v: 1, source: 'main', data: 'echo x\n' } }); // 本窗口回环
    expect(w).not.toHaveBeenCalled();
  });
});
