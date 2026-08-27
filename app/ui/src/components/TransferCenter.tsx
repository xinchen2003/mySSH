import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { retryHistoryTransfer, transferCmd, useTransferStore } from '../state/transfer-store';
import { usePanelWidth } from './panel-height';
import type { TransferHistoryView, TransferView } from '../term/types';
import { ConfirmDialog } from './ConfirmDialog';

/** 传输管理中心（批次六 5）：右侧抽屉，跨 session 聚合全部传输任务。
 *  SftpPanel 只保留摘要入口，完整列表与逐任务控制集中在此。
 *  打开时为当前窗口全部 session 标签建立订阅（transfer-store 惰性去重）。 */

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} K`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1048576).toFixed(1)} M`;
  return `${(n / 1073741824).toFixed(2)} G`;
}

const STATE_LABEL: Record<TransferView['state'], string> = {
  queued: '排队',
  running: '传输中',
  paused: '已暂停',
  done: '完成',
  failed: '失败',
  canceled: '已取消',
};

function baseName(path: string): string {
  const norm = path.replace(/\\/g, '/').replace(/\/+$/, '');
  return norm.split('/').pop() || path;
}

/** 历史记录行（失败/取消可一键重试续传；其余终态只读） */
function HistoryRow({ h, serverName }: { h: TransferHistoryView; serverName: string }) {
  const src = h.direction === 'upload' ? h.local : h.remote;
  const dst = h.direction === 'upload' ? h.remote : h.local;
  // SQLite UTC 时间串 → 本地 MM-DD HH:mm
  const d = new Date(`${h.updatedAt.replace(' ', 'T')}Z`);
  const p2 = (n: number) => String(n).padStart(2, '0');
  const time = Number.isNaN(d.getTime())
    ? h.updatedAt
    : `${p2(d.getMonth() + 1)}-${p2(d.getDate())} ${p2(d.getHours())}:${p2(d.getMinutes())}`;
  const color =
    h.state === 'done'
      ? 'text-green-500'
      : h.state === 'failed'
        ? 'text-red-400'
        : 'text-neutral-400';
  return (
    <div
      className="flex items-center gap-2 py-0.5 text-neutral-400"
      title={`${src}\n→ ${dst}\n${serverName}${h.error ? `\n${h.error}` : ''}`}
    >
      <span title={h.direction === 'upload' ? '上传' : '下载'}>
        {h.direction === 'upload' ? '⬆' : '⬇'}
      </span>
      <span className="min-w-0 flex-1 truncate text-neutral-200">
        {baseName(src)} → {baseName(dst)}
      </span>
      <span className="max-w-20 shrink-0 truncate text-neutral-400" title={serverName}>
        {serverName}
      </span>
      <span className={`shrink-0 ${color}`}>{STATE_LABEL[h.state] ?? h.state}</span>
      {(h.state === 'failed' || h.state === 'canceled') && (
        <button
          className="shrink-0 rounded px-1 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
          title="重试（断点续传）"
          onClick={() => void retryHistoryTransfer(h)}
        >
          ↻
        </button>
      )}
      <span className="w-20 shrink-0 text-right text-neutral-400">{time}</span>
    </div>
  );
}

function TransferRow({ t, sessionId }: { t: TransferView; sessionId: string }) {
  const [showErr, setShowErr] = useState(false);
  // 终态闪烁（批次十 1）：state 转移进 done/failed 时整行底色闪一次。
  // 渲染期派生重置（React 官方模式）；挂载即终态（历史回放帧）不闪。
  // 动画播完底色自动回透明，无需计时器清 class；再次转移时类名变化即重播。
  const [seen, setSeen] = useState<{ state: string; flash: 'done' | 'failed' | null }>({
    state: t.state,
    flash: null,
  });
  if (seen.state !== t.state) {
    setSeen({
      state: t.state,
      flash: t.state === 'done' || t.state === 'failed' ? t.state : null,
    });
  }
  const flash = seen.flash;
  // 0 字节文件完成时应显示 100%（bytesTotal=0 会让公式恒得 0，「0% 完成」误导）
  const pct =
    t.state === 'done'
      ? 100
      : t.bytesTotal > 0
        ? Math.min(100, (t.bytesDone / t.bytesTotal) * 100)
        : 0;
  // upload: 源=本地 目标=远程；download: 源=远程 目标=本地
  const src = t.direction === 'upload' ? t.local : t.remote;
  const dst = t.direction === 'upload' ? t.remote : t.local;
  const terminal = t.state === 'done' || t.state === 'failed' || t.state === 'canceled';
  const btn = 'rounded px-1 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200';
  return (
    <div
      className={`myssh-row-in -mx-1 rounded px-1 ${
        flash === 'done' ? 'myssh-flash-done' : flash === 'failed' ? 'myssh-flash-fail' : ''
      }`}
    >
      <div className="flex items-center gap-2 py-0.5 text-neutral-400">
        <span title={t.direction === 'upload' ? '上传' : '下载'}>
          {t.direction === 'upload' ? '⬆' : '⬇'}
        </span>
        <span className="min-w-0 flex-1 truncate text-neutral-200" title={`${src}\n→ ${dst}`}>
          {baseName(src)} → {baseName(dst)}
        </span>
        <div className="h-1.5 w-24 shrink-0 overflow-hidden rounded bg-neutral-800">
          <div
            className={`h-full transition-[width] duration-300 ${
              t.state === 'failed'
                ? 'bg-red-600'
                : t.state === 'done'
                  ? 'bg-green-600'
                  : t.state === 'running'
                    ? 'myssh-progress-running bg-blue-600'
                    : 'bg-blue-600'
            }`}
            style={{ width: `${pct}%` }}
          />
        </div>
        <span className="w-10 shrink-0 text-right">{pct.toFixed(0)}%</span>
        <span className="w-16 shrink-0 text-right text-neutral-400">
          {t.state === 'running' ? `${fmtSize(t.rate ?? 0)}/s` : STATE_LABEL[t.state]}
        </span>
        {!t.history && t.state === 'running' && (
          <button
            className={btn}
            title="暂停"
            onClick={() => void transferCmd(sessionId, 'transfer_pause', { transferId: t.id })}
          >
            ⏸
          </button>
        )}
        {!t.history && t.state === 'paused' && (
          <button
            className={btn}
            title="继续"
            onClick={() => void transferCmd(sessionId, 'transfer_resume', { transferId: t.id })}
          >
            ▶
          </button>
        )}
        {!t.history && (t.state === 'running' || t.state === 'queued' || t.state === 'paused') && (
          <button
            className={btn}
            title="取消"
            onClick={() => void transferCmd(sessionId, 'transfer_cancel', { transferId: t.id })}
          >
            ✕
          </button>
        )}
        {!t.history && (t.state === 'failed' || t.state === 'canceled') && (
          <button
            className={btn}
            title="重试（从断点续传）"
            onClick={() => void transferCmd(sessionId, 'transfer_retry', { transferId: t.id })}
          >
            ↻
          </button>
        )}
        {!t.history && t.error && (
          <button className={btn} title="查看错误" onClick={() => setShowErr((v) => !v)}>
            ⓘ
          </button>
        )}
        {!t.history && terminal && (
          <button
            className={btn}
            title="从队列移除"
            onClick={() => void transferCmd(sessionId, 'transfer_remove', { transferId: t.id })}
          >
            🗑
          </button>
        )}
        {t.history && <span className="shrink-0 text-neutral-400">上次</span>}
      </div>
      {showErr && t.error && <div className="ml-6 break-all py-0.5 text-red-400">{t.error}</div>}
    </div>
  );
}

export function TransferCenter() {
  const open = useTransferStore((s) => s.open);
  const setOpen = useTransferStore((s) => s.setOpen);
  const bySession = useTransferStore((s) => s.bySession);
  const tabs = useAppStore((s) => s.tabs);
  const history = useTransferStore((s) => s.history);
  const clearHistory = useTransferStore((s) => s.clearHistory);
  const sessions = useAppStore((s) => s.sessions);
  const [confirmClear, setConfirmClear] = useState(false);
  const { width, widthHandle } = usePanelWidth('ui.transferCenterWidth', 480);

  // Esc 关闭抽屉
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [open, setOpen]);

  if (!open) return null;

  const sessionIds = Object.keys(bySession).filter((id) => (bySession[id]?.length ?? 0) > 0);
  // 总进度（批次十 1）：全部活动任务的字节合计，标题栏下一条动画总条
  const active = Object.values(bySession)
    .flat()
    .filter((t) => t.state === 'queued' || t.state === 'running' || t.state === 'paused');
  const sumTotal = active.reduce((n, t) => n + t.bytesTotal, 0);
  const sumDone = active.reduce((n, t) => n + Math.min(t.bytesDone, t.bytesTotal), 0);
  const sumRate = active
    .filter((t) => t.state === 'running')
    .reduce((n, t) => n + (t.rate ?? 0), 0);
  const totalPct = sumTotal > 0 ? Math.min(100, (sumDone / sumTotal) * 100) : 0;
  const titleOf = (sid: string): string =>
    tabs.find((t) => t.target.kind === 'session' && t.target.sessionId === sid)?.title ?? sid;
  // 本次运行的 live 项与历史表同源（终态落表），按 id 去重避免重复展示
  const liveIds = new Set(
    Object.values(bySession)
      .flat()
      .map((t) => t.id),
  );
  const hist = history.filter((h) => !liveIds.has(h.id));
  const serverName = (sid: string) => sessions.find((x) => x.id === sid)?.name ?? sid;
  /** 全局批量操作：逐 session 下发（pause_all/resume_all/clear 均为会话级命令） */
  const forEachSession = (cmd: string, extra: Record<string, unknown> = {}) => {
    for (const sid of sessionIds) void transferCmd(sid, cmd, extra);
  };
  const qbtn =
    'rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200 disabled:opacity-40';

  return (
    <div
      className="fixed inset-y-0 right-0 z-40 flex flex-col border-l border-neutral-700 bg-neutral-900 text-xs text-neutral-200 shadow-xl"
      style={{ width }}
    >
      {widthHandle}
      <div className="flex items-center gap-2 border-b border-neutral-700 px-4 py-2">
        <span className="text-sm font-semibold text-neutral-100">传输管理</span>
        <button
          className={qbtn}
          disabled={sessionIds.length === 0}
          onClick={() => forEachSession('transfer_pause_all')}
        >
          全部暂停
        </button>
        <button
          className={qbtn}
          disabled={sessionIds.length === 0}
          onClick={() => forEachSession('transfer_resume_all')}
        >
          全部继续
        </button>
        <button
          className={qbtn}
          disabled={sessionIds.length === 0}
          onClick={() => forEachSession('transfer_clear', { filter: 'done' })}
        >
          清除已完成
        </button>
        <button
          className={qbtn}
          disabled={sessionIds.length === 0}
          onClick={() => forEachSession('transfer_clear', { filter: 'failed' })}
        >
          清除失败
        </button>
        <button
          className="ml-auto rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200"
          onClick={() => setOpen(false)}
          aria-label="关闭传输管理"
        >
          ✕
        </button>
      </div>
      {active.length > 0 && (
        <div className="flex items-center gap-2 border-b border-neutral-800 px-4 py-1.5">
          <div className="h-1.5 min-w-0 flex-1 overflow-hidden rounded bg-neutral-800">
            <div
              className="myssh-progress-running h-full bg-blue-600 transition-[width] duration-300"
              style={{ width: `${totalPct}%` }}
            />
          </div>
          <span className="shrink-0 text-neutral-400">
            {active.length} 项 · {fmtSize(sumDone)} / {fmtSize(sumTotal)}
            {sumRate > 0 ? ` · ${fmtSize(sumRate)}/s` : ''}
          </span>
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-2">
        {sessionIds.length === 0 && (
          <div className="px-1 py-4 text-neutral-400">（暂无传输任务）</div>
        )}
        {sessionIds.map((sid) => (
          <div key={sid} className="mb-2">
            <div className="border-b border-neutral-800 py-1 font-medium text-neutral-300">
              {titleOf(sid)}
            </div>
            {bySession[sid]?.map((t) => (
              <TransferRow key={t.id} t={t} sessionId={sid} />
            ))}
          </div>
        ))}
        <div className="mt-2 flex items-center gap-2 border-t border-neutral-800 py-1 font-medium text-neutral-300">
          历史记录
          <button
            className={qbtn}
            disabled={history.length === 0}
            onClick={() => setConfirmClear(true)}
          >
            清空
          </button>
        </div>
        {hist.length === 0 ? (
          <div className="px-1 py-2 text-neutral-400">（暂无历史记录）</div>
        ) : (
          hist.map((h) => <HistoryRow key={h.id} h={h} serverName={serverName(h.sessionId)} />)
        )}
      </div>
      {confirmClear && (
        <ConfirmDialog
          title="清空全部传输历史记录？"
          confirmLabel="清空"
          onCancel={() => setConfirmClear(false)}
          onConfirm={() => {
            setConfirmClear(false);
            void clearHistory();
          }}
        >
          <p className="text-neutral-300">共 {history.length} 条，跨全部服务器。</p>
          <p className="text-red-300">
            历史记录是断点续传的凭据，清空后未完成的传输无法跨重启恢复。
          </p>
        </ConfirmDialog>
      )}
    </div>
  );
}
