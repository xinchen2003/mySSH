import { Channel, invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import type { TransferHistoryView, TransferView } from '../term/types';
import { useAppStore } from './app-store';

/** 传输管理中心（批次六 5）：跨 session 聚合 transfer_subscribe 快照。
 *  订阅惰性建立：SftpPanel 打开时订自己的 session（ensureSession）；
 *  TransferCenter 打开时订当前窗口全部 session 标签（syncAllSessions）。
 *  每个 session 一条 Channel，重复订阅由 channels Map 去重（后端每订阅各起一个
 *  500ms 推送任务，去重避免空转）。
 *  顺带维护「打开 SFTP 时定位到终端 cwd」的导航请求（navRequests，SftpPanel 消费）。 */

interface TransferStore {
  /** sessionId → 传输快照（live + history 帧） */
  bySession: Record<string, TransferView[]>;
  /** 全部会话的持久化历史（transfers 表；TransferCenter 历史记录区） */
  history: TransferHistoryView[];
  /** 传输管理中心抽屉开关 */
  open: boolean;
  /** SFTP 导航请求：tabId → 远端目标路径（终端右键「打开 SFTP」面板已开时写入） */
  navRequests: Record<string, string>;
  setOpen(v: boolean): void;
  toggleOpen(): void;
  requestNav(tabId: string, path: string): void;
  consumeNav(tabId: string): void;
  /** 幂等：为 session 建立传输订阅（已订则跳过） */
  ensureSession(sessionId: string): void;
  /** 为当前窗口全部 session 标签建立订阅（TransferCenter 打开时调用） */
  syncAllSessions(): void;
  /** 拉取持久化历史（打开抽屉时、传输达终态后刷新） */
  loadHistory(): Promise<void>;
  /** 清空全部历史记录 */
  clearHistory(): Promise<void>;
}

/** sessionId → 订阅 Channel（模块级，不随 React 渲染重建） */
const channels = new Map<string, Channel<{ transfers: TransferView[] }>>();
/** sessionId → 上一帧各传输的状态（转移检测用；history 项不参与） */
const prevFrames = new Map<string, Map<string, string>>();

const ACTIVE_STATES = new Set(['queued', 'running', 'paused']);

/** 帧间状态转移 → 用户提示：开始（info）/完成（success）/失败（error），按方向聚合计数 */
function diffAndNotify(sessionId: string, transfers: TransferView[]): void {
  const cur = new Map<string, TransferView>();
  for (const t of transfers) if (!t.history) cur.set(t.id, t);
  const prev = prevFrames.get(sessionId);
  prevFrames.set(sessionId, new Map([...cur].map(([id, t]) => [id, t.state])));
  // 首帧只播种：订阅可能建立在传输进行中（另一窗口/面板先发起），误报「开始」比漏报更扰人
  if (!prev) return;
  let upStart = 0;
  let downStart = 0;
  let upDone = 0;
  let downDone = 0;
  const failed: TransferView[] = [];
  for (const [id, t] of cur) {
    const p = prev.get(id);
    if (!p) {
      if (t.state === 'queued' || t.state === 'running') {
        if (t.direction === 'upload') upStart++;
        else downStart++;
      } else if (t.state === 'failed') {
        // 入队即失败（帧间隔内跑完 queued→failed）：不能以「新出现」吞掉失败提示
        failed.push(t);
      }
    } else if (ACTIVE_STATES.has(p)) {
      if (t.state === 'done') {
        if (t.direction === 'upload') upDone++;
        else downDone++;
      } else if (t.state === 'failed') {
        failed.push(t);
      }
    }
  }
  const notify = useAppStore.getState().notify;
  if (upStart) notify(`开始上传 ${upStart} 项`, 'info');
  if (downStart) notify(`开始下载 ${downStart} 项`, 'info');
  if (upDone) notify(`${upDone} 项上传完成`, 'success');
  if (downDone) notify(`${downDone} 项下载完成`, 'success');
  if (failed.length > 0) {
    const first = failed[0];
    const name = first.remote || first.local;
    notify(
      failed.length === 1
        ? `传输失败：${name}${first.error ? `（${first.error}）` : ''}`
        : `${failed.length} 项传输失败（首个：${name}）`,
      'error',
    );
  }
  // 有任务达终态（已异步落 transfers 表）→ 刷新历史记录区
  if (upDone + downDone + failed.length > 0) void useTransferStore.getState().loadHistory();
}

/** 聚合发布全局活跃传输数（12.2 状态栏）；无订阅来源时置 null（不显示） */
function publishActive(bySession: Record<string, TransferView[]>): void {
  if (channels.size === 0) {
    useAppStore.getState().setTransferActive(null);
    return;
  }
  let n = 0;
  for (const list of Object.values(bySession)) {
    n += list.filter(
      (t) => !t.history && (t.state === 'queued' || t.state === 'running' || t.state === 'paused'),
    ).length;
  }
  useAppStore.getState().setTransferActive(n);
}

export const useTransferStore = create<TransferStore>((set, get) => ({
  bySession: {},
  history: [],
  open: false,
  navRequests: {},
  setOpen: (v) => {
    set({ open: v });
    if (v) {
      get().syncAllSessions();
      void get().loadHistory();
    }
  },
  toggleOpen: () => {
    const next = !get().open;
    set({ open: next });
    if (next) {
      get().syncAllSessions();
      void get().loadHistory();
    }
  },
  requestNav: (tabId, path) => set((s) => ({ navRequests: { ...s.navRequests, [tabId]: path } })),
  consumeNav: (tabId) =>
    set((s) => {
      if (!(tabId in s.navRequests)) return s;
      return {
        navRequests: Object.fromEntries(Object.entries(s.navRequests).filter(([k]) => k !== tabId)),
      };
    }),
  ensureSession: (sessionId) => {
    if (channels.has(sessionId)) return;
    const events = new Channel<{ transfers: TransferView[] }>();
    channels.set(sessionId, events);
    events.onmessage = (f) => {
      diffAndNotify(sessionId, f.transfers);
      set((s) => {
        const bySession = { ...s.bySession, [sessionId]: f.transfers };
        publishActive(bySession);
        return { bySession };
      });
    };
    // 历史帧（上次运行终态）：transfer_list 一次性合并，live 为准
    void invoke<{ transfers: TransferView[] }>('transfer_list', { sessionId })
      .then((r) => {
        set((s) => {
          const live = s.bySession[sessionId] ?? [];
          const liveIds = new Set(live.map((t) => t.id));
          const merged = [...live, ...r.transfers.filter((t) => !liveIds.has(t.id))];
          const bySession = { ...s.bySession, [sessionId]: merged };
          publishActive(bySession);
          return { bySession };
        });
      })
      .catch(() => undefined);
    void invoke('transfer_subscribe', { sessionId, events }).catch((e) => {
      channels.delete(sessionId);
      prevFrames.delete(sessionId);
      // E7006 = 会话记录已删但标签页还在（删服务器不关标签）：订阅无意义，静默跳过
      if (!String(e).includes('E7006')) {
        useAppStore.getState().notify(`传输订阅失败: ${e}`, 'warning');
      }
    });
  },
  syncAllSessions: () => {
    const { tabs, sessions } = useAppStore.getState();
    const known = new Set(sessions.map((s) => s.id));
    for (const t of tabs) {
      // 标签页可能引用已删除的会话（删服务器不关标签），跳过避免后端 E7006
      if (t.target.kind === 'session' && known.has(t.target.sessionId)) {
        get().ensureSession(t.target.sessionId);
      }
    }
  },
  loadHistory: async () => {
    try {
      const r = await invoke<{ records: TransferHistoryView[] }>('transfer_history');
      set({ history: r.records });
    } catch {
      // 历史加载失败静默：live 队列不受影响
    }
  },
  clearHistory: async () => {
    try {
      await invoke('transfer_history_clear');
      set({ history: [] });
      useAppStore.getState().notify('已清空传输历史', 'success');
    } catch (e) {
      useAppStore.getState().notify(`清空历史失败: ${String(e)}`, 'error');
    }
  },
}));

/** 传输控制命令（暂停/继续/取消/重试/移除/清理）；统一报错通知 */
export async function transferCmd(
  sessionId: string,
  cmd: string,
  extra: Record<string, unknown> = {},
): Promise<void> {
  try {
    await invoke(cmd, { sessionId, ...extra });
  } catch (e) {
    useAppStore.getState().notify(`操作失败: ${e}`, 'error');
  }
}
