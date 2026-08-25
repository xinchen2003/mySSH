import { Channel, invoke } from '@tauri-apps/api/core';
import { WebviewWindow } from '@tauri-apps/api/webviewWindow';
import { create } from 'zustand';
import { TerminalSession } from '../term/terminal-session';
import { GROUP_KEYS, readFailedList, readStringList, type FailedEntry } from './groups';
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
  TunnelInfo,
  SessionTunnelResult,
} from '../term/types';
import { tunnelDisplayName, tunnelFeedback } from './tunnel-utils';
import { reconnectRegistry } from '../term/registry';

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
  /** 待确认关闭的标签 id 列表（任一含活跃连接时汇总确认；单标签为单元素数组） */
  pendingCloseTabs: string[] | null;
  confirmCloseTab(): void;
  cancelCloseTab(): void;
  /** 请求关闭一组标签：确认守卫命中时汇总弹一次确认（§17.2 说明影响） */
  requestCloseTabs(ids: string[]): void;
  /** 断开标签全部连接但保留标签（pane 终态 closed，终端内容保留） */
  disconnectTab(id: string): void;
  /** 重连标签全部 pane（复用各 pane 在 TerminalView 注册的原位重连闭包） */
  reconnectTab(id: string): void;
  closeOtherTabs(id: string): void;
  closeTabsToRight(id: string): void;
  closeAllTabs(): void;
  /** 导入/导出（错误也走 notices） */
  importFrom(source: 'openssh' | 'putty' | 'xshell' | 'finalshell', path?: string): Promise<void>;
  exportConfig(encrypted: boolean, passphrase?: string): Promise<void>;
  importConfigFile(path: string, passphrase?: string): Promise<void>;
  connect(spec: TermOpenSpec): void;
  connectBySession(sessionId: string, title: string): void;
  /** 连接语义：已有该会话标签则激活，否则新标签 */
  connectOrActivate(sessionId: string, title: string): void;
  /** 标签分离：新窗口连接（TabBar ⧉ 与右键菜单共用） */
  connectInNewWindow(sessionId: string, title: string): void;
  /** 连接（或激活）并打开 SFTP 面板 */
  connectAndOpenSftp(sessionId: string, title: string): void;
  /** 复制服务器档案（新 id + 「副本」后缀） */
  duplicateSession(rec: SessionRecord): Promise<void>;
  /** 收藏切换（KV: sessions.favorites） */
  toggleFavorite(sessionId: string): void;
  /** 最近连接记录（KV: sessions.recent，cap 20，新→旧） */
  recordRecent(sessionId: string): void;
  /** 连接失败记录（KV: sessions.failed，cap 20）；成功连接时清除 */
  recordConnectFailure(sessionId: string, message: string): void;
  clearConnectFailure(sessionId: string): void;
  /** 分组 KV 集合更新（extras/collapsed 共用改写入口） */
  setGroupList(key: 'groups.extra' | 'groups.collapsed', list: string[]): void;
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
  stopTunnel(id: string): Promise<void>;
  loadTunnelDefs(): Promise<void>;
  /** 保存定义；start=true 时立即建立 */
  saveTunnel(def: TunnelDef, start: boolean): Promise<void>;
  deleteTunnel(id: string): Promise<void>;
  /** 复制定义（新 id，名称加「副本」，不启动） */
  duplicateTunnel(def: TunnelDef): Promise<void>;
  /** §9.6 连接反馈：随会话隧道启动结果汇总成通知 */
  notifySessionTunnels(sessionId: string, results: SessionTunnelResult[]): void;
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
  /** 标签活跃连接数（connected/connecting/reconnecting 计为活跃） */
  const countLive = (tab: Tab): number =>
    paneIds(tab.layout).filter((pid) => {
      const st = tab.panes[pid]?.state;
      return st === 'connected' || st === 'connecting' || st === 'reconnecting';
    }).length;

  /** 实际执行关标签：关闭全部 pane 会话并移除标签（确认守卫见 requestCloseTabs） */
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
      else if (ev.type === 'session_tunnels') get().notifySessionTunnels(ev.sessionId, ev.results);
      else {
        handleSessionState(set, tabId, id, ev);
        // 连接成功（含重连成功）→ 清掉该会话的「最近失败」记录
        if (ev.type === 'session_state' && ev.state === 'connected') {
          const tab = get().tabs.find((t) => t.id === tabId);
          if (tab && tab.target.kind === 'session') get().clearConnectFailure(tab.target.sessionId);
        }
      }
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
    duplicateTunnel: async (def) => {
      await get().saveTunnel(
        { ...def, id: `td-${crypto.randomUUID()}`, name: `${tunnelDisplayName(def)} 副本` },
        false,
      );
      get().notify('隧道已复制（未启动）', 'success');
    },

    notifySessionTunnels: (sessionId, results) => {
      const name = get().sessions.find((s) => s.id === sessionId)?.name ?? sessionId;
      const fb = tunnelFeedback(name, results);
      if (fb) get().notify(fb.message, fb.level);
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
      get().recordRecent(sessionId);
    },
    connectOrActivate: (sessionId, title) => {
      const existing = get().tabs.find(
        (t) => t.target.kind === 'session' && t.target.sessionId === sessionId,
      );
      if (existing) set({ activeId: existing.id });
      else get().connectBySession(sessionId, title);
    },

    connectInNewWindow: (sessionId, title) => {
      const label = `det-${sessionId}`.replace(/[^a-zA-Z0-9-]/g, '-');
      const win = new WebviewWindow(label, {
        url: `index.html?detach=${encodeURIComponent(sessionId)}`,
        title: `${title} · mySSH`,
        width: 1200,
        height: 800,
      });
      void win.once('tauri://error', () => undefined);
    },

    connectAndOpenSftp: (sessionId, title) => {
      get().connectOrActivate(sessionId, title);
      const id = get().activeId;
      if (id && !get().sftpOpen[id]) get().toggleSftp(id);
    },

    duplicateSession: async (rec) => {
      const copy: SessionRecord = {
        ...rec,
        id: crypto.randomUUID(),
        name: `${rec.name} 副本`,
        jumpChain: [...rec.jumpChain],
        tags: [...rec.tags],
      };
      try {
        await invoke('session_upsert', { record: copy });
        await get().loadSessions();
        get().notify(`已复制为「${copy.name}」（凭据不随档案复制）`, 'success');
      } catch (e) {
        get().notify(`复制服务器失败: ${String(e)}`, 'error');
      }
    },

    toggleFavorite: (sessionId) => {
      const cur = new Set(readStringList(get().settings[GROUP_KEYS.favorites]));
      const had = cur.has(sessionId);
      if (had) cur.delete(sessionId);
      else cur.add(sessionId);
      get().setSetting(GROUP_KEYS.favorites, [...cur]);
      get().notify(had ? '已取消收藏' : '已收藏', 'success');
    },

    recordRecent: (sessionId) => {
      const cur = readStringList(get().settings[GROUP_KEYS.recent]);
      const next = [sessionId, ...cur.filter((id) => id !== sessionId)].slice(0, 20);
      get().setSetting(GROUP_KEYS.recent, next);
    },

    recordConnectFailure: (sessionId, message) => {
      const cur = readFailedList(get().settings[GROUP_KEYS.failed]);
      const next: FailedEntry[] = [
        { id: sessionId, message, ts: Date.now() },
        ...cur.filter((f) => f.id !== sessionId),
      ].slice(0, 20);
      get().setSetting(GROUP_KEYS.failed, next);
    },

    clearConnectFailure: (sessionId) => {
      const cur = readFailedList(get().settings[GROUP_KEYS.failed]);
      if (cur.some((f) => f.id === sessionId))
        get().setSetting(
          GROUP_KEYS.failed,
          cur.filter((f) => f.id !== sessionId),
        );
    },

    setGroupList: (key, list) => get().setSetting(key, [...new Set(list)]),

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
    pendingCloseTabs: null,
    confirmCloseTab: () => {
      const ids = get().pendingCloseTabs;
      set({ pendingCloseTabs: null });
      if (ids) for (const id of ids) doCloseTab(id);
    },
    cancelCloseTab: () => set({ pendingCloseTabs: null }),

    requestCloseTabs: (ids) => {
      const { tabs, settings } = get();
      const want = new Set(ids);
      const targets = tabs.filter((t) => want.has(t.id));
      if (targets.length === 0) return;
      const live = targets.reduce((n, t) => n + countLive(t), 0);
      if (settings['terminal.confirmCloseTab'] !== false && live > 0) {
        set({ pendingCloseTabs: targets.map((t) => t.id) });
        return;
      }
      for (const t of targets) doCloseTab(t.id);
    },

    disconnectTab: (id) => {
      const tab = get().tabs.find((t) => t.id === id);
      if (!tab) return;
      for (const pid of paneIds(tab.layout)) {
        const p = tab.panes[pid];
        if (!p) continue;
        if (p.state === 'connected' || p.state === 'connecting' || p.state === 'reconnecting') {
          void p.session.close();
          get().setPaneState(id, pid, 'closed');
        }
      }
    },

    reconnectTab: (id) => {
      const tab = get().tabs.find((t) => t.id === id);
      if (!tab) return;
      for (const pid of paneIds(tab.layout)) reconnectRegistry.get(pid)?.();
    },

    closeOtherTabs: (id) =>
      get().requestCloseTabs(
        get()
          .tabs.filter((t) => t.id !== id)
          .map((t) => t.id),
      ),

    closeTabsToRight: (id) => {
      const idx = get().tabs.findIndex((t) => t.id === id);
      if (idx < 0) return;
      get().requestCloseTabs(
        get()
          .tabs.slice(idx + 1)
          .map((t) => t.id),
      );
    },

    closeAllTabs: () =>
      get().requestCloseTabs(get().tabs.map((t) => t.id)),

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

    /** 关标签守卫（批次一 7.6）：并入 requestCloseTabs（单标签即单元素数组） */
    closeTab: (id) => get().requestCloseTabs([id]),

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
