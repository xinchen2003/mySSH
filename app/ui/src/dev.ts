import { invoke } from '@tauri-apps/api/core';
import { useAppStore, type DockTab } from './state/app-store';
import { termRegistry } from './term/registry';
import type { SessionRecord, TermOpenSpec } from './term/types';

/**
 * 冒烟/自动化钩子（CDP 驱动用）。与图形界面同一入口，不走旁路。
 */
export function installDevHooks(): void {
  const w = window as unknown as { __myssh?: unknown };
  w.__myssh = {
    rawInvoke: invoke,
    connect: (spec: TermOpenSpec) => useAppStore.getState().connect(spec),
    connectBySession: (sessionId: string, title: string) =>
      useAppStore.getState().connectBySession(sessionId, title),
    loadSessions: () => useAppStore.getState().loadSessions(),
    upsertSession: (record: SessionRecord) => invoke('session_upsert', { record }),
    credSet: (sessionId: string, kind: 'password' | 'keyPassphrase', secret: string) =>
      invoke('cred_set', { sessionId, kind, secret }),
    sessions: () => useAppStore.getState().sessions,
    splitActive: (dir: 'row' | 'col') => useAppStore.getState().splitActive(dir),
    tabs: () =>
      useAppStore.getState().tabs.map((t) => ({
        id: t.id,
        title: t.title,
        activePaneId: t.activePaneId,
        panes: Object.values(t.panes).map((p) => ({
          id: p.id,
          state: p.state,
          backendTabId: p.session.tabId,
        })),
      })),
    /** 激活 tab 的激活 pane 的 xterm 实例 */
    activeTerm: () => {
      const s = useAppStore.getState();
      const tab = s.tabs.find((t) => t.id === s.activeId);
      return tab ? (termRegistry.get(tab.activePaneId) ?? null) : null;
    },
    /** 读激活终端可见缓冲区文本（验证渲染内容） */
    activeBufferText: () => {
      const s = useAppStore.getState();
      const tab = s.tabs.find((t2) => t2.id === s.activeId);
      const term = tab ? termRegistry.get(tab.activePaneId) : null;
      if (!term) return null;
      const buf = term.buffer.active;
      const lines: string[] = [];
      for (let i = 0; i < buf.length; i++)
        lines.push(buf.getLine(i)?.translateToString(true) ?? '');
      return lines.join('\n');
    },
    /** 向激活终端注入输入（与键盘同路径：xterm paste → onData → term_input） */
    type: (text: string) => {
      const s = useAppStore.getState();
      const tab = s.tabs.find((t) => t.id === s.activeId);
      const term = tab ? termRegistry.get(tab.activePaneId) : null;
      term?.paste(text);
    },
    pendingHostKey: () => useAppStore.getState().pendingHostKeys[0] ?? null,
    /** 当前通知堆叠（分级冒烟断言用） */
    notices: () => useAppStore.getState().notices,
    /** 待确认关闭的标签 id 列表（关标签确认冒烟断言用） */
    pendingCloseTabs: () => useAppStore.getState().pendingCloseTabs,
    /** 待确认删除的会话（删除确认冒烟断言用） */
    pendingDeleteSession: () => useAppStore.getState().pendingDeleteSession,
    /** 隧道运行态 / 定义（批次三冒烟断言用） */
    tunnels: () => useAppStore.getState().tunnels,
    tunnelDefs: () => useAppStore.getState().tunnelDefs,
    loadTunnelDefs: () => useAppStore.getState().loadTunnelDefs(),
    toggleDock: (tab: DockTab) => useAppStore.getState().toggleDock(tab),
    /** 应答 hostkey 弹窗（与点按钮同路径） */
    answerHostKey: async (accept: boolean, remember: boolean) => {
      const p = useAppStore.getState().pendingHostKeys[0] ?? null;
      if (!p) return false;
      await invoke('hostkey_confirm', { confirmId: p.confirmId, accept, remember });
      useAppStore.getState().shiftHostKey();
      return true;
    },
  };
}
