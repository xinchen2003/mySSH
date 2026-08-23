import { Channel, invoke } from '@tauri-apps/api/core';
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
  ConnectTarget,
  HostKeyPromptFrame,
  KiChallengeFrame,
  SessionRecord,
  SessionStateFrame,
  TermEvent,
  TermOpenSpec,
  TunnelDef,
  TunnelForm,
  TunnelInfo,
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
  /** 分屏新 pane 复用同一连接目标 */
  target: ConnectTarget;
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

  openConnect(editTarget?: SessionRecord): void;
  closeConnect(): void;
  /** 命令面板（Ctrl+Shift+P） */
  paletteOpen: boolean;
  togglePalette(): void;
  /** 瞬态通知（toast）；null 清除 */
  notice: string | null;
  notify(msg: string): void;
  /** 导入/导出（错误也走 notice） */
  importFrom(source: 'openssh' | 'putty' | 'xshell' | 'finalshell', path?: string): Promise<void>;
  exportConfig(encrypted: boolean, passphrase?: string): Promise<void>;
  importConfigFile(path: string, passphrase?: string): Promise<void>;
  connect(spec: TermOpenSpec): void;
  connectBySession(sessionId: string, title: string): void;
  /** 会话档案 CRUD（秘密经 cred_set 单独进保险库） */
  loadSessions(): Promise<void>;
  deleteSession(id: string): Promise<void>;
  /** 编辑既有会话（null=新建） */
  editing: SessionRecord | null;
  sessions: SessionRecord[];
  sidebarOpen: boolean;
  toggleSidebar(): void;

  /** 内部：订阅去重标记 */
  _tunnelsSubscribed: boolean;
  /** SFTP 面板：tabId → 是否打开 */
  sftpOpen: Record<string, boolean>;
  toggleSftp(tabId: string): void;
  /** 监控面板：tabId → 是否打开（与 SFTP 互斥，共用底栏位） */
  metricsOpen: Record<string, boolean>;
  toggleMetrics(tabId: string): void;
  /** 隧道面板 */
  tunnels: TunnelInfo[];
  /** 持久化隧道定义 */
  tunnelDefs: TunnelDef[];
  tunnelPanelOpen: boolean;
  toggleTunnelPanel(): void;
  /** 1Hz 订阅（App 挂载时调用一次；重复调用幂等） */
  subscribeTunnels(): void;
  startTunnel(form: TunnelForm): Promise<string>;
  stopTunnel(id: string): Promise<void>;
  loadTunnelDefs(): Promise<void>;
  /** 保存定义；start=true 时立即建立 */
  saveTunnel(def: TunnelDef, start: boolean): Promise<void>;
  deleteTunnel(id: string): Promise<void>;
  splitActive(dir: 'row' | 'col'): void;
  closePane(tabId: string, paneId: string): void;
  setActive(id: string): void;
  /** 拖拽重排：把 dragId 移到 targetId 之前 */
  moveTab(dragId: string, targetId: string): void;
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

  const openTabWithTarget = (target: ConnectTarget, title: string) => {
    const tabId = `tab${tabSeq++}`;
    const pane = makePane(tabId);
    const tab: Tab = {
      id: tabId,
      title,
      target,
      layout: leaf(pane.id),
      panes: { [pane.id]: pane },
      activePaneId: pane.id,
    };
    set((s) => ({ tabs: [...s.tabs, tab], activeId: tabId, showConnect: false }));
  };

  return {
    tabs: [],
    activeId: null,
    showConnect: false,
    pendingHostKeys: [],
    pendingKis: [],
    editing: null,
    sessions: [],
    sidebarOpen: true,
    tunnels: [],
    tunnelDefs: [],
    tunnelPanelOpen: false,
    _tunnelsSubscribed: false,
    sftpOpen: {},
    metricsOpen: {},

    toggleTunnelPanel: () => set((s) => ({ tunnelPanelOpen: !s.tunnelPanelOpen })),

    toggleSftp: (tabId) =>
      set((s) => ({
        sftpOpen: { ...s.sftpOpen, [tabId]: !s.sftpOpen[tabId] },
        metricsOpen: { ...s.metricsOpen, [tabId]: false },
      })),

    toggleMetrics: (tabId) =>
      set((s) => ({
        metricsOpen: { ...s.metricsOpen, [tabId]: !s.metricsOpen[tabId] },
        sftpOpen: { ...s.sftpOpen, [tabId]: false },
      })),

    subscribeTunnels: () => {
      if (get()._tunnelsSubscribed) return;
      set({ _tunnelsSubscribed: true });
      const events = new Channel<{ tunnels: TunnelInfo[] }>();
      events.onmessage = (frame) => set({ tunnels: frame.tunnels });
      void invoke('tunnel_subscribe', { events });
    },

    startTunnel: async (form) => {
      const res = await invoke<{ tunnelId: string }>('tunnel_start', {
        spec: {
          sessionId: form.sessionId,
          kind: form.kind,
          bindHost: form.bindHost,
          bindPort: form.bindPort,
          targetHost: form.targetHost ?? null,
          targetPort: form.targetPort ?? null,
          failFast: false,
        },
      });
      return res.tunnelId;
    },

    stopTunnel: async (id) => {
      await invoke('tunnel_stop', { tunnelId: id });
    },

    loadTunnelDefs: async () => {
      const defs = await invoke<TunnelDef[]>('tunnel_defs');
      set({ tunnelDefs: defs });
    },

    saveTunnel: async (def, start) => {
      await invoke('tunnel_save', { def: { ...def, start } });
      await get().loadTunnelDefs();
    },

    deleteTunnel: async (id) => {
      await invoke('tunnel_delete', { tunnelId: id });
      await get().loadTunnelDefs();
    },

    toggleSidebar: () => set((s) => ({ sidebarOpen: !s.sidebarOpen })),

    loadSessions: async () => {
      const sessions = await invoke<SessionRecord[]>('session_list');
      set({ sessions });
    },

    deleteSession: async (id) => {
      await invoke('session_delete', { sessionId: id });
      await get().loadSessions();
    },

    connectBySession: (sessionId, title) => {
      openTabWithTarget({ kind: 'session', sessionId }, title);
    },

    openConnect: (editTarget) => set({ showConnect: true, editing: editTarget ?? null }),
    closeConnect: () => set({ showConnect: false, editing: null }),

    paletteOpen: false,
    togglePalette: () => set((s) => ({ paletteOpen: !s.paletteOpen })),
    notice: null,
    notify: (msg) => {
      set({ notice: msg });
      setTimeout(() => {
        if (get().notice === msg) set({ notice: null });
      }, 4000);
    },

    importFrom: async (source, path) => {
      try {
        const cmd = {
          openssh: 'import_openssh',
          putty: 'import_putty',
          xshell: 'import_xshell',
          finalshell: 'import_finalshell',
        }[source];
        const r = await invoke<{ imported: number; skipped: number; unresolvedJumps?: number }>(
          cmd,
          { path: path ?? null },
        );
        await get().loadSessions();
        let msg = `导入 ${r.imported} 条会话`;
        if (r.skipped) msg += `，跳过 ${r.skipped}`;
        if (r.unresolvedJumps) msg += `，${r.unresolvedJumps} 个跳板引用待手工补链`;
        get().notify(msg);
      } catch (e) {
        get().notify(`导入失败: ${String(e)}`);
      }
    },

    exportConfig: async (encrypted, passphrase) => {
      try {
        const r = await invoke<{ path: string }>('config_export', {
          encrypted,
          passphrase: passphrase ?? null,
        });
        get().notify(`已导出: ${r.path}`);
      } catch (e) {
        get().notify(`导出失败: ${String(e)}`);
      }
    },

    importConfigFile: async (path, passphrase) => {
      try {
        const r = await invoke<{ sessions: number; tunnels: number; credentials: number }>(
          'config_import',
          { path, passphrase: passphrase ?? null },
        );
        await get().loadSessions();
        await get().loadTunnelDefs();
        get().notify(`导入完成: ${r.sessions} 会话 / ${r.tunnels} 隧道 / ${r.credentials} 凭据`);
      } catch (e) {
        get().notify(`导入失败: ${String(e)}`);
      }
    },

    connect: (spec) => {
      openTabWithTarget({ kind: 'spec', spec }, `${spec.user}@${spec.host}`);
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

    moveTab: (dragId, targetId) =>
      set((s) => {
        if (dragId === targetId) return {};
        const from = s.tabs.findIndex((t) => t.id === dragId);
        if (from < 0) return {};
        const tabs = [...s.tabs];
        const [moved] = tabs.splice(from, 1);
        const to = tabs.findIndex((t) => t.id === targetId);
        tabs.splice(to < 0 ? tabs.length : to, 0, moved);
        return { tabs };
      }),

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
