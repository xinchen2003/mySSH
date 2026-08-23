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
/** 通知分级（批次一 7.7）：success/info 短时自动消失，warning 较长，error 常驻手动关 */
export type NotificationLevel = 'success' | 'info' | 'warning' | 'error';

export interface Notice {
  id: number;
  level: NotificationLevel;
  message: string;
}

/** 各级别自动消失时长（ms）；null = 常驻手动关闭 */
const NOTICE_TTL: Record<NotificationLevel, number | null> = {
  success: 3000,
  info: 4000,
  warning: 8000,
  error: null,
};

/** 堆叠上限：溢出时优先丢弃最旧的非 error 通知 */
const MAX_NOTICES = 5;

let noticeSeq = 1;
/** 一次性自动消失定时器（非空转轮询；手动关闭时清理） */
const noticeTimers = new Map<number, number>();

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
/** 多窗口隔离（M5 标签分离）：窗口 label 作 id 前缀，防跨窗口 tab/pane id 碰撞 */
let idPrefix = '';
/** App 挂载时以窗口 label 初始化一次 */
export function initIdPrefix(windowLabel: string): void {
  idPrefix = windowLabel === 'main' ? '' : `${windowLabel}-`;
}

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
  /** 分级通知堆叠（toast） */
  notices: Notice[];
  notify(message: string, level?: NotificationLevel): void;
  dismissNotice(id: number): void;
  /** 待确认删除的会话档案（删除会级联清凭据，必须确认） */
  pendingDeleteSession: SessionRecord | null;
  requestDeleteSession(rec: SessionRecord): void;
  confirmDeleteSession(): Promise<void>;
  cancelDeleteSession(): void;
  /** 待确认关闭的标签 id（有活跃连接时需确认） */
  pendingCloseTab: string | null;
  confirmCloseTab(): void;
  cancelCloseTab(): void;
  /** 导入/导出（错误也走 notices） */
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
  /** 应用设置（settings KV 全量缓存；启动时 loadSettings 拉一次） */
  settings: Record<string, unknown>;
  settingsLoaded: boolean;
  loadSettings(): Promise<void>;
  /** 乐观本地更新 + 落库（theme/terminal 键的副作用由 App 效果层统一应用） */
  setSetting(key: string, value: unknown): void;
  /** 设置面板开关 */
  settingsOpen: boolean;
  toggleSettings(): void;
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
  /** 实际执行关标签：关闭全部 pane 会话并移除标签（确认守卫见 closeTab） */
  const doCloseTab = (id: string) => {
    const { tabs, activeId } = get();
    const tab = tabs.find((t) => t.id === id);
    if (tab) for (const pid of paneIds(tab.layout)) void tab.panes[pid]?.session.close();
    const next = tabs.filter((t) => t.id !== id);
    set({
      tabs: next,
      activeId: activeId === id ? (next[next.length - 1]?.id ?? null) : activeId,
    });
  };

  const makePane = (tabId: string): Pane => {
    const id = `${idPrefix}p${paneSeq++}`;
    const onEvent = (ev: TermEvent) => {
      if (ev.type === 'hostkey_prompt')
        set((s) => ({ pendingHostKeys: [...s.pendingHostKeys, ev] }));
      else if (ev.type === 'ki_challenge') set((s) => ({ pendingKis: [...s.pendingKis, ev] }));
      else handleSessionState(set, tabId, id, ev);
    };
    return { id, session: new TerminalSession(onEvent), state: 'connecting' };
  };

  const openTabWithTarget = (target: ConnectTarget, title: string) => {
    const tabId = `${idPrefix}tab${tabSeq++}`;
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
    settings: {},
    settingsLoaded: false,
    settingsOpen: false,

    toggleSettings: () => set((s) => ({ settingsOpen: !s.settingsOpen })),

    loadSettings: async () => {
      const res = await invoke<{ settings: Record<string, unknown> }>('settings_list');
      set({ settings: res.settings, settingsLoaded: true });
    },

    setSetting: (key, value) => {
      set((s) => ({ settings: { ...s.settings, [key]: value } }));
      invoke('settings_set', { key, value }).catch(() => undefined);
    },

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
    notices: [],
    notify: (message, level = 'info') => {
      const id = noticeSeq++;
      set((s) => {
        const notices = [...s.notices, { id, level, message }];
        while (notices.length > MAX_NOTICES) {
          const idx = notices.findIndex((n) => n.level !== 'error');
          const [dropped] = notices.splice(idx >= 0 ? idx : 0, 1);
          const t = noticeTimers.get(dropped.id);
          if (t !== undefined) {
            clearTimeout(t);
            noticeTimers.delete(dropped.id);
          }
        }
        return { notices };
      });
      const ttl = NOTICE_TTL[level];
      if (ttl !== null) {
        noticeTimers.set(
          id,
          setTimeout(() => {
            noticeTimers.delete(id);
            get().dismissNotice(id);
          }, ttl),
        );
      }
    },
    dismissNotice: (id) => {
      const t = noticeTimers.get(id);
      if (t !== undefined) {
        clearTimeout(t);
        noticeTimers.delete(id);
      }
      set((s) => ({ notices: s.notices.filter((n) => n.id !== id) }));
    },
    pendingDeleteSession: null,
    requestDeleteSession: (rec) => set({ pendingDeleteSession: rec }),
    cancelDeleteSession: () => set({ pendingDeleteSession: null }),
    confirmDeleteSession: async () => {
      const rec = get().pendingDeleteSession;
      if (!rec) return;
      set({ pendingDeleteSession: null });
      try {
        await get().deleteSession(rec.id);
        get().notify(`已删除服务器「${rec.name}」`, 'success');
      } catch (e) {
        get().notify(`删除服务器失败: ${String(e)}`, 'error');
      }
    },
    pendingCloseTab: null,
    confirmCloseTab: () => {
      const id = get().pendingCloseTab;
      set({ pendingCloseTab: null });
      if (id) doCloseTab(id);
    },
    cancelCloseTab: () => set({ pendingCloseTab: null }),

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
        get().notify(msg, r.unresolvedJumps ? 'warning' : 'success');
      } catch (e) {
        get().notify(`导入失败: ${String(e)}`, 'error');
      }
    },

    exportConfig: async (encrypted, passphrase) => {
      try {
        const r = await invoke<{ path: string }>('config_export', {
          encrypted,
          passphrase: passphrase ?? null,
        });
        get().notify(`已导出: ${r.path}`, 'success');
      } catch (e) {
        get().notify(`导出失败: ${String(e)}`, 'error');
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
        get().notify(
          `导入完成: ${r.sessions} 会话 / ${r.tunnels} 隧道 / ${r.credentials} 凭据`,
          'success',
        );
      } catch (e) {
        get().notify(`导入失败: ${String(e)}`, 'error');
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
      const layout = removeLeaf(tab.layout, paneId);
      if (!layout) {
        // 最后一叶：走整标签关闭入口（含活跃连接确认守卫；
        // session 统一由确认后的 doCloseTab 关闭，避免取消确认留下僵尸 pane）
        get().closeTab(tabId);
        return;
      }
      const pane = tab.panes[paneId];
      if (pane) void pane.session.close();
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

    /** 关标签守卫（批次一 7.6）：设置开启且有活跃连接时先弹确认；全部已关闭则直接关 */
    closeTab: (id) => {
      const { tabs, settings } = get();
      const tab = tabs.find((t) => t.id === id);
      if (!tab) return;
      const live = paneIds(tab.layout).filter((pid) => {
        const st = tab.panes[pid]?.state;
        return st === 'connected' || st === 'connecting' || st === 'reconnecting';
      }).length;
      if (settings['terminal.confirmCloseTab'] !== false && live > 0) {
        set({ pendingCloseTab: id });
        return;
      }
      doCloseTab(id);
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
