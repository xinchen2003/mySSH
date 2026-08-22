import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from './state/app-store';
import { termRegistry } from './term/registry';
import type { TermOpenSpec } from './term/types';

/**
 * 冒烟/自动化钩子（CDP 驱动用）。与图形界面同一入口，不走旁路。
 */
export function installDevHooks(): void {
  const w = window as unknown as { __myssh?: unknown };
  w.__myssh = {
    connect: (spec: TermOpenSpec) => useAppStore.getState().connect(spec),
    tabs: () =>
      useAppStore.getState().tabs.map((t) => ({
        id: t.id,
        title: t.title,
        state: t.state,
        backendTabId: t.session.tabId,
      })),
    /** 读激活终端可见缓冲区文本（验证渲染内容） */
    activeBufferText: () => {
      const id = useAppStore.getState().activeId;
      const term = id ? termRegistry.get(id) : null;
      if (!term) return null;
      const buf = term.buffer.active;
      const lines: string[] = [];
      for (let i = 0; i < buf.length; i++)
        lines.push(buf.getLine(i)?.translateToString(true) ?? '');
      return lines.join('\n');
    },
    /** 向激活终端注入输入（与键盘同路径：xterm paste → onData → term_input） */
    type: (text: string) => {
      const id = useAppStore.getState().activeId;
      const term = id ? termRegistry.get(id) : null;
      term?.paste(text);
    },
    pendingHostKey: () => useAppStore.getState().pendingHostKey,
    /** 应答 hostkey 弹窗（与点按钮同路径） */
    answerHostKey: async (accept: boolean, remember: boolean) => {
      const p = useAppStore.getState().pendingHostKey;
      if (!p) return false;
      await invoke('hostkey_confirm', { confirmId: p.confirmId, accept, remember });
      useAppStore.getState().setPendingHostKey(null);
      return true;
    },
  };
}
