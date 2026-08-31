import { Channel, invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import type { TransferHistoryView, TransferView } from '../term/types';
import { useAppStore } from './app-store';
import { tNow } from '../i18n';

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
  /** 传输中心可视开关（dock「传输中心」页签由 app-store openDock/closeDock 同步此字段） */
  open: boolean;
  /** SFTP 导航请求：tabId → 远端目标路径（终端右键「打开 SFTP」面板已开时写入） */
  navRequests: Record<string, string>;
  setOpen(v: boolean): void;
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
      } else if (t.state === 'done') {
        // 亚帧完成（局域网小文件整个生命周期 < 500ms 推送间隔）：
        // 此前不入任何计数 → 零提示，用户完全无感知
        if (t.direction === 'upload') upDone++;
        else downDone++;
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
  if (upStart) notify(tNow('state.uploadStarted', { count: upStart }), 'info');
  if (downStart) notify(tNow('state.downloadStarted', { count: downStart }), 'info');
  if (upDone) notify(tNow('state.uploadDone', { count: upDone }), 'success');
  if (downDone) notify(tNow('state.downloadDone', { count: downDone }), 'success');
  if (failed.length > 0) {
    const first = failed[0];
    const name = first.remote || first.local;
    notify(
      failed.length === 1
        ? first.error
          ? tNow('state.transferFailedWithError', { name, error: first.error })
          : tNow('state.transferFailed', { name })
        : tNow('state.transfersFailed', { count: failed.length, name }),
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
        useAppStore
          .getState()
          .notify(tNow('state.subscribeFailed', { error: String(e) }), 'warning');
      }
    });
  },
  syncAllSessions: () => {
    const { tabs, sessions } = useAppStore.getState();
    const byId = new Map(sessions.map((s) => [s.id, s]));
    for (const t of tabs) {
      if (t.target.kind !== 'session') continue;
      const rec = byId.get(t.target.sessionId);
      // 跳过已删档案（后端 E7006）与本地会话（无 SSH 通道，订阅必失败）
      if (rec && rec.kind !== 'local') get().ensureSession(t.target.sessionId);
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
      useAppStore.getState().notify(tNow('state.historyCleared'), 'success');
    } catch (e) {
      useAppStore
        .getState()
        .notify(tNow('state.clearHistoryFailed', { error: String(e) }), 'error');
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
    useAppStore.getState().notify(tNow('state.operationFailed', { error: String(e) }), 'error');
  }
}
/** 父目录（本地 \ 统一按 / 处理；盘符根 C:/ 的父级是其自身） */
function parentPath(p: string, remote: boolean): string {
  const norm = p.replace(/\\/g, '/').replace(/\/+$/, '');
  const idx = norm.lastIndexOf('/');
  if (idx < 0) return remote ? '/' : '';
  if (idx === 0) return '/';
  if (idx === 2 && norm[1] === ':') return norm.slice(0, 3);
  return norm.slice(0, idx);
}

/** 历史行一键重试（批次十一 2）：按记录的方向/路径重新入队（onExists=resume 断点续传）。
 *  历史记录是逐文件完整路径，而 sftp_upload/download 的目标参数是目录，
 *  故上传取 remote 父目录、下载取 local 父目录。会话档案已删则不可重试
 *  （ensure_ctx 需从档案解析凭据）；成功后确保订阅存在以便看到进度。 */
export async function retryHistoryTransfer(h: TransferHistoryView): Promise<void> {
  const app = useAppStore.getState();
  if (!app.sessions.some((s) => s.id === h.sessionId)) {
    app.notify(tNow('state.originDeleted'), 'error');
    return;
  }
  try {
    if (h.direction === 'upload') {
      await invoke('sftp_upload', {
        sessionId: h.sessionId,
        local: h.local,
        remote: parentPath(h.remote, true),
        onExists: 'resume',
      });
    } else {
      await invoke('sftp_download', {
        sessionId: h.sessionId,
        remote: h.remote,
        local: parentPath(h.local, false),
        onExists: 'resume',
      });
    }
    useTransferStore.getState().ensureSession(h.sessionId);
    app.notify(tNow('state.requeued'), 'success');
  } catch (e) {
    app.notify(tNow('state.retryFailed', { error: String(e) }), 'error');
  }
}
