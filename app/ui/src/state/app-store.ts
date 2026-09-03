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
import { broadcastInput } from '../term/broadcast';
import { useTransferStore } from './transfer-store';
import { tNow } from '../i18n';

export type PaneState = 'connecting' | 'connected' | 'reconnecting' | 'closed' | 'error';
/** 底部 dock 页签（DevTools 风格工具面板）；单值天然互斥 */
export type DockTab = 'sftp' | 'metrics' | 'tunnel' | 'transfer';
/** 通知分级（批次一 7.7）：success/info 短时自动消失，warning 较长，error 常驻手动关 */
export type NotificationLevel = 'success' | 'info' | 'warning' | 'error';

export interface Notice {
  id: number;
  level: NotificationLevel;
  message: string;
  /** 可序列化动作（12.1）：label 展示，actionId 经 notice-actions 注册表分发，arg 为上下文 */
  action?: { label: string; actionId: string; arg?: string };
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
/** 本窗口 label（广播跨窗口事件帧 source 字段/回环防护；live binding） */
export let windowLabel = 'main';
/** App 挂载时以窗口 label 初始化一次 */
export function initIdPrefix(label: string): void {
  windowLabel = label;
  idPrefix = label === 'main' ? '' : `${label}-`;
}

// Shell 集成不注入：曾向远程 shell 写 stty/OSC 7 钩子（PROMPT_COMMAND/precmd）
// 与分屏 cd，在密码过期强制改密等登录交互里会污染输入造成死循环。OSC 7 仅被动
// 解析（terminal-session.ts）：服务器 shell 原生上报时「跟随终端目录」仍可用。

/** 目标是否为本地会话（session 档案 kind==='local'）；spec/档案缺失一律按远程处理。
 *  本地会话禁用：SFTP/监控/隧道等 SSH 专属功能 */
export function isLocalTarget(target: ConnectTarget): boolean {
  if (target.kind !== 'session') return false;
  // 建标签时已定死 kind，直接读；旧标签（无 sessionKind）回落查表
  if (target.sessionKind) return target.sessionKind === 'local';
  const rec = useAppStore.getState().sessions.find((r) => r.id === target.sessionId);
  return rec?.kind === 'local';
}

interface AppStore {
  tabs: Tab[];
  activeId: string | null;
  showConnect: boolean;
  /** 决策帧队列：多 pane 并发弹窗时逐个处理 */
  pendingHostKeys: HostKeyPromptFrame[];
  pendingKis: KiChallengeFrame[];

  /** 打开连接对话框；editTarget=编辑既有会话，presetGroup=预填分组路径（分组菜单「新建连接」） */
  openConnect(editTarget?: SessionRecord, presetGroup?: string): void;
  closeConnect(): void;
  /** 命令面板（Ctrl+Shift+P） */
  paletteOpen: boolean;
  togglePalette(): void;
  /** 分级通知堆叠（toast）；12.1：可选可序列化动作（actionId 经 notice-actions 注册表分发） */
  notices: Notice[];
  notify(
    message: string,
    level?: NotificationLevel,
    action?: { label: string; actionId: string; arg?: string },
  ): void;
  dismissNotice(id: number): void;
  /** 终端 bell 待读标签（12.6）：激活即清 */
  bellTabs: string[];
  markBell(tabId: string): void;
  /** 全局传输活跃数（12.2 状态栏）：由 SFTP 面板现有订阅顺带发布；null=无订阅来源不显示 */
  transferActive: number | null;
  setTransferActive(n: number | null): void;
  /** 快速连接对话框（12.5 空态；不保存档案的临时连接） */
  quickConnectOpen: boolean;
  toggleQuickConnect(): void;
  /** 断开单个 pane（12.4 命令面板「断开当前连接」；终态 closed，终端内容保留） */
  disconnectPane(tabId: string, paneId: string): void;
  /** 待确认删除的会话档案（删除会级联清凭据，必须确认） */
  pendingDeleteSession: SessionRecord | null;
  requestDeleteSession(rec: SessionRecord): void;
  confirmDeleteSession(): Promise<void>;
  cancelDeleteSession(): void;
  /** 待确认关闭的标签 id 列表（任一含活跃连接时汇总确认；单标签为单元素数组） */
  pendingCloseTabs: string[] | null;
  confirmCloseTab(): void;
  cancelCloseTab(): void;
  /** 已关闭标签栈（批次十一：Ctrl+Shift+T 重开；仅 session 类标签可重连，新→旧，cap 10） */
  closedTabs: { sessionId: string; title: string }[];
  /** 弹栈重开最近关闭的标签（复用 connectBySession 重连） */
  reopenClosedTab(): void;
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
  exportConfig(encrypted: boolean, passphrase?: string): Promise<void>;
  importConfigFile(path: string, passphrase?: string): Promise<void>;
  connect(spec: TermOpenSpec): void;
  connectBySession(sessionId: string, title: string): void;
  /** 连接语义：已有该会话标签则激活，否则新标签 */
  connectOrActivate(sessionId: string, title: string): void;
  /** 标签分离：新窗口连接（TabBar ⧉ 与右键菜单共用） */
  connectInNewWindow(sessionId: string, title: string): void;
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
  /** 新建连接时预填的分组路径（null=不预填） */
  connectPreset: string | null;
  sessions: SessionRecord[];
  sidebarOpen: boolean;
  toggleSidebar(): void;

  /** 内部：订阅去重标记 */
  _tunnelsSubscribed: boolean;
  /** 底部 dock（窗口级）：当前激活的工具页签；null = 收起。
   *  单值天然互斥（替代原 sftpOpen/metricsOpen 逐标签互斥）。
   *  'transfer' 与 transfer-store.open 联动（openDock/closeDock 内同步，订阅逻辑不变） */
  dockTab: DockTab | null;
  openDock(tab: DockTab): void;
  closeDock(): void;
  /** 同页签再次触发 = 关闭 */
  toggleDock(tab: DockTab): void;
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
  /** 广播输入开关（11）：开启后输入同步写入本窗口全部已连接 pane，并经事件转发其它窗口 */
  broadcastEnabled: boolean;
  toggleBroadcast(): void;
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
    set((s) => ({
      tabs: next,
      activeId: activeId === id ? (next[next.length - 1]?.id ?? null) : activeId,
      bellTabs: s.bellTabs.filter((t) => t !== id),
      // 批次十一：session 类标签压入已关闭栈（cap 10），供 Ctrl+Shift+T 重开；
      // 快速连接（spec）无档案可重连，不入栈
      closedTabs:
        tab && tab.target.kind === 'session'
          ? [...s.closedTabs, { sessionId: tab.target.sessionId, title: tab.title }].slice(-10)
          : s.closedTabs,
    }));
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
          // 连接成功（含重连）不向远程 shell 注入任何字节——批次六的 stty/OSC 7 钩子
          // 与分屏 cd 已移除：密码过期强制改密等登录交互会被注入污染成死循环
        }
      }
    };
    const session = new TerminalSession(onEvent);
    // 广播输入（11）：输入帧旁路钩子，开关状态在触发时读取
    session.inputHook = (data) => {
      if (get().broadcastEnabled) broadcastInput(tabId, id, data);
    };
    return { id, session, state: 'connecting' };
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
    connectPreset: null,
    sessions: [],
    sidebarOpen: true,
    tunnels: [],
    tunnelDefs: [],
    _tunnelsSubscribed: false,
    dockTab: null,
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

    openDock: (tab) => {
      // 防线：本地会话无 SSH 通道，任何调用路径（含漏检入口）都不许打开
      const active = get().tabs.find((t) => t.id === get().activeId);
      if ((tab === 'sftp' || tab === 'metrics') && active && isLocalTarget(active.target)) {
        get().notify(
          tab === 'sftp' ? tNow('state.localNoSftp') : tNow('state.localNoMetrics'),
          'warning',
        );
        return;
      }
      set({ dockTab: tab });
      // 传输中心内容仍订阅 transfer-store.open（保持订阅建立逻辑不变）
      useTransferStore.getState().setOpen(tab === 'transfer');
    },

    closeDock: () => {
      set({ dockTab: null });
      useTransferStore.getState().setOpen(false);
    },

    toggleDock: (tab) => {
      if (get().dockTab === tab) get().closeDock();
      else get().openDock(tab);
    },

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
        {
          ...def,
          id: `td-${crypto.randomUUID()}`,
          name: tNow('state.copyName', { name: tunnelDisplayName(def) }),
        },
        false,
      );
      get().notify(tNow('state.tunnelDuplicated'), 'success');
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
      const rec = get().sessions.find((r) => r.id === sessionId);
      openTabWithTarget(
        {
          kind: 'session',
          sessionId,
          sessionKind: rec?.kind === 'local' ? 'local' : 'ssh',
          encoding: rec?.encoding ?? null,
        },
        title,
      );
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
        // 与主窗口 tauri.conf.json 一致开启原生拖放（批次十三：真实路径 + 冲突确认）
        dragDropEnabled: true,
      });
      void win.once('tauri://error', () => undefined);
    },

    duplicateSession: async (rec) => {
      // 名称自动去重：「X 副本」「X 副本 2」…
      const names = new Set(get().sessions.map((x) => x.name));
      let name = tNow('state.copyName', { name: rec.name });
      for (let i = 2; names.has(name); i++)
        name = tNow('state.copyNameN', { name: rec.name, n: i });
      const copy: SessionRecord = {
        ...rec,
        id: crypto.randomUUID(),
        name,
        jumpChain: [...rec.jumpChain],
        tags: [...rec.tags],
      };
      try {
        await invoke('session_upsert', { record: copy });
        // 端口转发随档案克隆（新 id、绑定新会话、不启动）；
        // 凭据存保险库按会话 id 取且不可回读，按现有模型不复制，需重新录入
        // tunnelDefs 可能尚未加载（隧道弹层从未打开过），先拉取再筛，否则静默丢克隆
        await get().loadTunnelDefs();
        const tunnels = get().tunnelDefs.filter((t) => t.sessionId === rec.id);
        for (const t of tunnels) {
          await invoke('tunnel_save', {
            def: { ...t, id: `td-${crypto.randomUUID()}`, sessionId: copy.id, start: false },
          });
        }
        await get().loadSessions();
        if (tunnels.length > 0) await get().loadTunnelDefs();
        get().notify(
          tunnels.length > 0
            ? tNow('state.sessionDuplicatedWithTunnels', { name, count: tunnels.length })
            : tNow('state.sessionDuplicated', { name }),
          'success',
        );
      } catch (e) {
        get().notify(tNow('state.duplicateSessionFailed', { error: String(e) }), 'error');
      }
    },

    toggleFavorite: (sessionId) => {
      const cur = new Set(readStringList(get().settings[GROUP_KEYS.favorites]));
      const had = cur.has(sessionId);
      if (had) cur.delete(sessionId);
      else cur.add(sessionId);
      get().setSetting(GROUP_KEYS.favorites, [...cur]);
      get().notify(had ? tNow('state.unfavorited') : tNow('state.favorited'), 'success');
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

    openConnect: (editTarget, presetGroup) =>
      set({ showConnect: true, editing: editTarget ?? null, connectPreset: presetGroup ?? null }),
    closeConnect: () => set({ showConnect: false, editing: null, connectPreset: null }),

    paletteOpen: false,
    togglePalette: () => set((s) => ({ paletteOpen: !s.paletteOpen })),
    broadcastEnabled: false,
    toggleBroadcast: () => set((s) => ({ broadcastEnabled: !s.broadcastEnabled })),
    quickConnectOpen: false,
    toggleQuickConnect: () => set((s) => ({ quickConnectOpen: !s.quickConnectOpen })),
    bellTabs: [],
    markBell: (tabId) =>
      set((s) => (s.bellTabs.includes(tabId) ? {} : { bellTabs: [...s.bellTabs, tabId] })),
    transferActive: null,
    setTransferActive: (n) => set({ transferActive: n }),
    disconnectPane: (tabId, paneId) => {
      const tab = get().tabs.find((t) => t.id === tabId);
      const pane = tab?.panes[paneId];
      if (!tab || !pane) return;
      if (
        pane.state === 'connected' ||
        pane.state === 'connecting' ||
        pane.state === 'reconnecting'
      ) {
        void pane.session.close();
        get().setPaneState(tabId, paneId, 'closed');
      }
    },
    notices: [],
    notify: (message, level = 'info', action) => {
      const id = noticeSeq++;
      set((s) => {
        const notices = [...s.notices, { id, level, message, action }];
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
        get().notify(tNow('state.sessionDeleted', { name: rec.name }), 'success');
      } catch (e) {
        get().notify(tNow('state.deleteSessionFailed', { error: String(e) }), 'error');
      }
    },
    pendingCloseTabs: null,
    confirmCloseTab: () => {
      const ids = get().pendingCloseTabs;
      set({ pendingCloseTabs: null });
      if (ids) for (const id of ids) doCloseTab(id);
    },
    cancelCloseTab: () => set({ pendingCloseTabs: null }),
    closedTabs: [],
    reopenClosedTab: () => {
      const stack = get().closedTabs;
      const top = stack[stack.length - 1];
      if (!top) return;
      set({ closedTabs: stack.slice(0, -1) });
      // 档案可能已删除：connectBySession 照常开标签，连接失败走既有 error 终态
      get().connectBySession(top.sessionId, top.title);
    },

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

    closeAllTabs: () => get().requestCloseTabs(get().tabs.map((t) => t.id)),

    exportConfig: async (encrypted, passphrase) => {
      try {
        const r = await invoke<{ path: string }>('config_export', {
          encrypted,
          passphrase: passphrase ?? null,
        });
        // 12.1：通知附加动作（actionId 注册表分发，arg 为导出文件路径）
        get().notify(tNow('state.exported', { path: r.path }), 'success', {
          label: tNow('state.openInExplorer'),
          actionId: 'open-in-explorer',
          arg: r.path,
        });
      } catch (e) {
        get().notify(tNow('state.exportFailed', { error: String(e) }), 'error');
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
          tNow('state.importDone', {
            sessions: r.sessions,
            tunnels: r.tunnels,
            credentials: r.credentials,
          }),
          'success',
        );
      } catch (e) {
        get().notify(tNow('state.importFailed', { error: String(e) }), 'error');
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
      // 6b：新 pane 复用 tab.target（TerminalView 以同一 target attach = 同服务器新通道）。
      // 不再注入 cd 继承源 pane cwd（注入移除，见 makePane 内说明）
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

    // 激活即清 bell 待读标记（12.6）
    setActive: (id) => set((s) => ({ activeId: id, bellTabs: s.bellTabs.filter((t) => t !== id) })),

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
