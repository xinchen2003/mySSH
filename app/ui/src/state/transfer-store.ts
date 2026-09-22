import { Channel, invoke } from '@tauri-apps/api/core';
import { create } from 'zustand';
import type { TransferHistoryView, TransferJobView, TransferView } from '../term/types';
import { useAppStore } from './app-store';
import { tNow } from '../i18n';

/** 传输管理中心（批次六 5）：跨 session 聚合 transfer_subscribe 事件流。
 *  PR-9 增量事件协议：首帧 snapshot（每实体当前 eventSeq）→ 之后 tick-diff 只发
 *  变化的 upsert/remove；upsert 为全量实体状态（乱序/重复/丢失可收敛）。
 *  序号缺口/代际不符/心跳超时（看门狗 15s）→ 拆订阅重建（snapshot 重同步）。
 *  store 有界：session 级终态传输留 200、终态目录任务留 50；完整历史只分页查询。
 *  订阅惰性建立：SftpPanel 打开时订自己的 session（ensureSession）；
 *  TransferCenter 打开时订当前窗口全部 session 标签（syncAllSessions）。
 *  顺带维护「打开 SFTP 时定位到终端 cwd」的导航请求（navRequests，SftpPanel 消费）。 */

/** 事件信封（与后端 TransferEvent 对齐；id 带 t:/j: 前缀） */
export interface TransferEventJson {
  id: string;
  kind: 'transfer' | 'job';
  eventType: 'upsert' | 'remove';
  generation: number;
  eventSeq: number;
  payload: TransferView | TransferJobView | null;
}

export type TransferFrame =
  | { type: 'snapshot'; generation: number; events: TransferEventJson[] }
  | { type: 'events'; generation: number; events: TransferEventJson[] }
  | { type: 'heartbeat'; generation: number };

/** sessionId → 订阅 Channel（模块级，不随 React 渲染重建） */
const channels = new Map<string, Channel<TransferFrame>>();
/** sessionId → 订阅状态（代际/序号簿/最近帧时间） */
const subs = new Map<
  string,
  { generation: number; seqs: Record<string, number>; lastFrameAt: number }
>();
/** sessionId → 上一帧各传输的状态（转移检测用；history 项不参与） */
const prevFrames = new Map<string, Map<string, string>>();
/** sessionId → 上一帧各目录任务的状态 */
const prevJobs = new Map<string, Map<string, string>>();

const ACTIVE_STATES = new Set(['queued', 'running', 'paused']);
/** store 有界（PR-9）：session 级终态条目上限 */
const MAX_TERMINAL_TRANSFERS = 200;
const MAX_TERMINAL_JOBS = 50;
/** 看门狗：超过此时长无任何帧（含心跳）即判订阅死亡，拆重建 */
const WATCHDOG_STALE_MS = 15_000;

/** 终态条目裁剪（PR-9 前端 store 有界）：保留全部非终态 + 末尾 cap 条终态 */
export function trimTerminal<T extends { state: string }>(
  list: T[],
  terminalStates: Set<string>,
  cap: number,
): T[] {
  let terminal = 0;
  for (const t of list) if (terminalStates.has(t.state)) terminal++;
  if (terminal <= cap) return list;
  const keep: T[] = [];
  let budget = cap;
  // 从尾部留 cap 条终态；非终态全保留
  const tailTerminal = new Set<number>();
  for (let i = list.length - 1; i >= 0 && budget > 0; i--) {
    if (terminalStates.has(list[i].state)) {
      tailTerminal.add(i);
      budget--;
    }
  }
  for (let i = 0; i < list.length; i++) {
    if (!terminalStates.has(list[i].state) || tailTerminal.has(i)) keep.push(list[i]);
  }
  return keep;
}

const TRANSFER_TERMINAL = new Set(['done', 'failed', 'canceled']);
const JOB_TERMINAL = new Set(['completed', 'failed', 'canceled']);

export type ApplyResult =
  | { ok: true; transfers: TransferView[]; jobs: TransferJobView[] }
  | { ok: false; reason: 'gap' | 'stale-generation' };

/** 纯函数：把一帧 events 应用到（transfers, jobs）列表。
 *  序号规则：eventSeq <= last 丢弃（重复/迟到）；> last+1 判缺口（调用方重建）；
 *  upsert 全量替换/追加，remove 按 id 删除。乱序重复不破坏状态（PR-9 验收）。 */
export function applyTransferEvents(
  cur: { transfers: TransferView[]; jobs: TransferJobView[] },
  seqs: Record<string, number>,
  events: TransferEventJson[],
  generation: number,
): ApplyResult {
  let transfers = cur.transfers;
  let jobs = cur.jobs;
  let tDirty = false;
  let jDirty = false;
  for (const e of events) {
    if (e.generation !== generation) return { ok: false, reason: 'stale-generation' };
    const last = seqs[e.id] ?? 0;
    if (e.eventSeq <= last) continue;
    if (e.eventSeq > last + 1) return { ok: false, reason: 'gap' };
    seqs[e.id] = e.eventSeq;
    if (e.kind === 'transfer') {
      if (!tDirty) {
        transfers = [...transfers];
        tDirty = true;
      }
      const rawId = e.id.slice(2);
      const idx = transfers.findIndex((t) => t.id === rawId);
      if (e.eventType === 'remove') {
        if (idx >= 0) transfers.splice(idx, 1);
      } else {
        const v = e.payload as TransferView;
        if (idx >= 0) transfers[idx] = v;
        else transfers.push(v);
      }
    } else {
      if (!jDirty) {
        jobs = [...jobs];
        jDirty = true;
      }
      const rawId = e.id.slice(2);
      const idx = jobs.findIndex((j) => j.id === rawId);
      if (e.eventType === 'remove') {
        if (idx >= 0) jobs.splice(idx, 1);
      } else {
        const v = e.payload as TransferJobView;
        if (idx >= 0) jobs[idx] = v;
        else jobs.push(v);
      }
    }
  }
  return { ok: true, transfers, jobs };
}

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

/** 目录任务终态转移 → 聚合提示（首帧播种不报） */
function jobDiffAndNotify(sessionId: string, jobs: TransferJobView[]): void {
  const cur = new Map(jobs.map((j) => [j.id, j]));
  const prev = prevJobs.get(sessionId);
  prevJobs.set(sessionId, new Map([...cur].map(([id, j]) => [id, j.state])));
  if (!prev) return;
  let done = 0;
  let failed = 0;
  for (const [id, j] of cur) {
    const p = prev.get(id);
    if (!p || p === j.state) continue;
    if (j.state === 'completed') done++;
    else if (j.state === 'failed') failed++;
  }
  const notify = useAppStore.getState().notify;
  if (done) notify(tNow('state.dirJobDone', { count: done }), 'success');
  if (failed) notify(tNow('state.dirJobFailed', { count: failed }), 'error');
  if (done + failed > 0) void useTransferStore.getState().loadHistory();
}

/** 聚合发布全局活跃传输数（12.2 状态栏）；无订阅来源时置 null（不显示） */
function publishActive(
  bySession: Record<string, TransferView[]>,
  jobsBySession: Record<string, TransferJobView[]>,
): void {
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
  // 目录任务：非终态即活跃（一个 job 计 1）
  for (const list of Object.values(jobsBySession)) {
    n += list.filter(
      (j) => j.state === 'scanning' || j.state === 'transferring' || j.state === 'finalizing',
    ).length;
  }
  useAppStore.getState().setTransferActive(n);
}

/** 拆订阅重建（snapshot 重同步：序号缺口/代际漂移/心跳超时统一走这里） */
function rebuildSession(sessionId: string): void {
  channels.delete(sessionId);
  subs.delete(sessionId);
  prevFrames.delete(sessionId);
  prevJobs.delete(sessionId);
  useTransferStore.getState().ensureSession(sessionId);
}

let watchdogStarted = false;
function startWatchdog(): void {
  if (watchdogStarted) return;
  watchdogStarted = true;
  setInterval(() => {
    const now = Date.now();
    for (const [sid, sub] of subs) {
      if (now - sub.lastFrameAt > WATCHDOG_STALE_MS) rebuildSession(sid);
    }
  }, 5_000);
}

interface TransferStore {
  /** sessionId → 传输快照（live + history 帧） */
  bySession: Record<string, TransferView[]>;
  /** sessionId → 目录任务快照（PR-8 DirectoryJob） */
  jobsBySession: Record<string, TransferJobView[]>;
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
  /** 历史回放行的本地移除：DB 行删除无事件流，transfer_remove 成功后前端自行下账 */
  dropTransfer(sessionId: string, id: string): void;
}

export const useTransferStore = create<TransferStore>((set, get) => ({
  bySession: {},
  jobsBySession: {},
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
    startWatchdog();
    const events = new Channel<TransferFrame>();
    channels.set(sessionId, events);
    subs.set(sessionId, { generation: 0, seqs: {}, lastFrameAt: Date.now() });
    events.onmessage = (f) => {
      const sub = subs.get(sessionId);
      if (!sub) return;
      if (f.type === 'heartbeat') {
        // 代际不符的心跳同样说明对端状态异常，判活即可（事件帧才校验代际）
        sub.lastFrameAt = Date.now();
        return;
      }
      sub.lastFrameAt = Date.now();
      if (f.type === 'snapshot') {
        // snapshot 重同步：整表替换 + 序号簿重置（snapshotSeq 语义）
        const transfers: TransferView[] = [];
        const jobs: TransferJobView[] = [];
        sub.seqs = {};
        sub.generation = f.generation;
        for (const e of f.events) {
          sub.seqs[e.id] = e.eventSeq;
          if (e.eventType === 'upsert' && e.payload) {
            if (e.kind === 'transfer') transfers.push(e.payload as TransferView);
            else jobs.push(e.payload as TransferJobView);
          }
        }
        set((s) => {
          const bySession = { ...s.bySession, [sessionId]: transfers };
          const jobsBySession = { ...s.jobsBySession, [sessionId]: jobs };
          publishActive(bySession, jobsBySession);
          return { bySession, jobsBySession };
        });
        // 播种转移检测（重同步帧不报）
        prevFrames.set(
          sessionId,
          new Map(transfers.filter((t) => !t.history).map((t) => [t.id, t.state])),
        );
        prevJobs.set(sessionId, new Map(jobs.map((j) => [j.id, j.state])));
        return;
      }
      // events 帧：代际校验在 apply 内逐事件执行
      const state = get();
      const r = applyTransferEvents(
        {
          transfers: state.bySession[sessionId] ?? [],
          jobs: state.jobsBySession[sessionId] ?? [],
        },
        sub.seqs,
        f.events,
        sub.generation,
      );
      if (!r.ok) {
        if (r.reason === 'gap') rebuildSession(sessionId); // 序号缺口 → snapshot 重同步
        return; // stale-generation：旧代际迟到帧，丢弃
      }
      const transfers = trimTerminal(r.transfers, TRANSFER_TERMINAL, MAX_TERMINAL_TRANSFERS);
      const jobs = trimTerminal(r.jobs, JOB_TERMINAL, MAX_TERMINAL_JOBS);
      set((s) => {
        const bySession = { ...s.bySession, [sessionId]: transfers };
        const jobsBySession = { ...s.jobsBySession, [sessionId]: jobs };
        publishActive(bySession, jobsBySession);
        return { bySession, jobsBySession };
      });
      diffAndNotify(sessionId, transfers);
      jobDiffAndNotify(sessionId, jobs);
    };
    // 历史帧（上次运行终态）：transfer_list 一次性合并，live 为准
    void invoke<{ transfers: TransferView[] }>('transfer_list', { sessionId })
      .then((r) => {
        set((s) => {
          const live = s.bySession[sessionId] ?? [];
          const liveIds = new Set(live.map((t) => t.id));
          const merged = [...live, ...r.transfers.filter((t) => !liveIds.has(t.id))];
          const bySession = { ...s.bySession, [sessionId]: merged };
          publishActive(bySession, s.jobsBySession);
          return { bySession };
        });
      })
      .catch(() => undefined);
    void invoke('transfer_subscribe', { sessionId, events }).catch((e) => {
      channels.delete(sessionId);
      subs.delete(sessionId);
      prevFrames.delete(sessionId);
      prevJobs.delete(sessionId);
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
  dropTransfer: (sessionId, id) =>
    set((s) => {
      const cur = s.bySession[sessionId];
      if (!cur) return {};
      const bySession = {
        ...s.bySession,
        [sessionId]: cur.filter((t) => t.id !== id),
      };
      publishActive(bySession, s.jobsBySession);
      return { bySession };
    }),
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

/** 目录任务控制命令（PR-8；transfer_job_pause/resume/cancel/retry/remove） */
export async function transferJobCmd(sessionId: string, cmd: string, jobId: string): Promise<void> {
  await transferCmd(sessionId, cmd, { jobId });
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
  } catch (e) {
    app.notify(tNow('state.operationFailed', { error: String(e) }), 'error');
  }
}
