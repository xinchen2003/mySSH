import { create } from 'zustand';
import { TerminalSession } from '../term/terminal-session';
import {
  firstLeaf,
  leaf,
  paneIds,
  removeLeaf,
  setRatio,
  splitLeaf,
  type LayoutNode,
} from '../term/layout';
import type {
  HostKeyPromptFrame,
  KiChallengeFrame,
  SessionStateFrame,
  TermEvent,
  TermOpenSpec,
} from '../term/types';

export type PaneState = 'connecting' | 'connected' | 'reconnecting' | 'closed' | 'error';

export interface Pane {
  id: string;
  session: TerminalSession;
  state: PaneState;
}

export interface Tab {
  /** 本地稳定 id（React key） */
  id: string;
  title: string;
  /** 分屏新 pane 复用同一连接参数 */
  spec: TermOpenSpec;
  layout: LayoutNode;
  panes: Record<string, Pane>;
  activePaneId: string;
}

let tabSeq = 1;
let paneSeq = 1;

interface AppStore {
  tabs: Tab[];
  activeId: string | null;
  showConnect: boolean;
  /** 决策帧队列：多 pane 并发弹窗时逐个处理 */
  pendingHostKeys: HostKeyPromptFrame[];
  pendingKis: KiChallengeFrame[];

  openConnect(): void;
  closeConnect(): void;
  connect(spec: TermOpenSpec): void;
  splitActive(dir: 'row' | 'col'): void;
  closePane(tabId: string, paneId: string): void;
  setActive(id: string): void;
  setActivePane(tabId: string, paneId: string): void;
  setSplitRatio(tabId: string, splitId: string, ratio: number): void;
  setPaneState(tabId: string, paneId: string, state: PaneState): void;
  closeTab(id: string): void;
  shiftHostKey(): void;
  shiftKi(): void;
}

export const useAppStore = create<AppStore>((set, get) => {
  const makePane = (tabId: string): Pane => {
    const id = `p${paneSeq++}`;
    const onEvent = (ev: TermEvent) => {
      if (ev.type === 'hostkey_prompt')
        set((s) => ({ pendingHostKeys: [...s.pendingHostKeys, ev] }));
      else if (ev.type === 'ki_challenge') set((s) => ({ pendingKis: [...s.pendingKis, ev] }));
      else handleSessionState(set, tabId, id, ev);
    };
    return { id, session: new TerminalSession(onEvent), state: 'connecting' };
  };

  return {
    tabs: [],
    activeId: null,
    showConnect: false,
    pendingHostKeys: [],
    pendingKis: [],

    openConnect: () => set({ showConnect: true }),
    closeConnect: () => set({ showConnect: false }),

    connect: (spec) => {
      const tabId = `tab${tabSeq++}`;
      const pane = makePane(tabId);
      const tab: Tab = {
        id: tabId,
        title: `${spec.user}@${spec.host}`,
        spec,
        layout: leaf(pane.id),
        panes: { [pane.id]: pane },
        activePaneId: pane.id,
      };
      set((s) => ({ tabs: [...s.tabs, tab], activeId: tabId, showConnect: false }));
    },

    splitActive: (dir) => {
      const { tabs, activeId } = get();
      const tab = tabs.find((t) => t.id === activeId);
      if (!tab) return;
      const pane = makePane(tab.id);
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tab.id
            ? {
                ...t,
                layout: splitLeaf(t.layout, t.activePaneId, dir, pane.id),
                panes: { ...t.panes, [pane.id]: pane },
                activePaneId: pane.id,
              }
            : t,
        ),
      }));
    },

    closePane: (tabId, paneId) => {
      const { tabs } = get();
      const tab = tabs.find((t) => t.id === tabId);
      if (!tab) return;
      const pane = tab.panes[paneId];
      if (pane) void pane.session.close();
      const layout = removeLeaf(tab.layout, paneId);
      if (!layout) {
        // 最后一叶：整标签关闭
        get().closeTab(tabId);
        return;
      }
      const panes = Object.fromEntries(Object.entries(tab.panes).filter(([k]) => k !== paneId));
      const activePaneId = tab.activePaneId === paneId ? firstLeaf(layout) : tab.activePaneId;
      set((s) => ({
        tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, layout, panes, activePaneId } : t)),
      }));
    },

    setActive: (id) => set({ activeId: id }),

    setActivePane: (tabId, paneId) =>
      set((s) => ({
        tabs: s.tabs.map((t) => (t.id === tabId ? { ...t, activePaneId: paneId } : t)),
      })),

    setSplitRatio: (tabId, splitId, ratio) =>
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tabId ? { ...t, layout: setRatio(t.layout, splitId, ratio) } : t,
        ),
      })),

    setPaneState: (tabId, paneId, state) =>
      set((s) => ({
        tabs: s.tabs.map((t) =>
          t.id === tabId
            ? {
                ...t,
                panes: {
                  ...t.panes,
                  [paneId]: { ...t.panes[paneId], state },
                },
              }
            : t,
        ),
      })),

    closeTab: (id) => {
      const { tabs, activeId } = get();
      const tab = tabs.find((t) => t.id === id);
      if (tab) for (const pid of paneIds(tab.layout)) void tab.panes[pid]?.session.close();
      const next = tabs.filter((t) => t.id !== id);
      set({
        tabs: next,
        activeId: activeId === id ? (next[next.length - 1]?.id ?? null) : activeId,
      });
    },

    shiftHostKey: () => set((s) => ({ pendingHostKeys: s.pendingHostKeys.slice(1) })),
    shiftKi: () => set((s) => ({ pendingKis: s.pendingKis.slice(1) })),
  };
});

function handleSessionState(
  set: (fn: (s: AppStore) => Partial<AppStore>) => void,
  tabId: string,
  paneId: string,
  ev: SessionStateFrame,
): void {
  set((s) => ({
    tabs: s.tabs.map((t) =>
      t.id === tabId
        ? { ...t, panes: { ...t.panes, [paneId]: { ...t.panes[paneId], state: ev.state } } }
        : t,
    ),
  }));
}
