import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
  Channel: class {
    onmessage: unknown = null;
  },
}));

import { invoke } from '@tauri-apps/api/core';
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
    pendingCloseTab: null,
    pendingDeleteSession: null,
    settings: {},
    sessions: [],
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
    expect(state().pendingCloseTab).toBe(id);

    state().confirmCloseTab();
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTab).toBeNull();
  });

  it('取消确认后标签保留', () => {
    const s = state();
    s.connect(spec);
    const id = state().tabs[0].id;

    s.closeTab(id);
    state().cancelCloseTab();
    expect(state().tabs).toHaveLength(1);
    expect(state().pendingCloseTab).toBeNull();
  });

  it('设置关闭（terminal.confirmCloseTab=false）时直接关闭', () => {
    useAppStore.setState({ settings: { 'terminal.confirmCloseTab': false } });
    const s = state();
    s.connect(spec);
    s.closeTab(state().tabs[0].id);
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTab).toBeNull();
  });

  it('全部 pane 已关闭时直接关闭（不弹确认）', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];
    s.setPaneState(tab.id, tab.activePaneId, 'closed');

    s.closeTab(tab.id);
    expect(state().tabs).toHaveLength(0);
    expect(state().pendingCloseTab).toBeNull();
  });

  it('closePane 关闭最后一叶也走确认守卫', () => {
    const s = state();
    s.connect(spec);
    const tab = state().tabs[0];

    s.closePane(tab.id, tab.activePaneId);
    expect(state().tabs).toHaveLength(1); // 守卫拦截，标签未关
    expect(state().pendingCloseTab).toBe(tab.id);

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
    expect(state().pendingCloseTab).toBeNull(); // 非末叶不触发关标签确认
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
