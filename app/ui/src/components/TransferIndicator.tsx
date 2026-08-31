import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { useTransferStore } from '../state/transfer-store';
import { useT } from '../i18n';

/** 数字部分格式化（模块级缓存；观感同 toFixed(1)/toFixed(2)） */
const sizeNum1 = new Intl.NumberFormat('en-US', {
  minimumFractionDigits: 1,
  maximumFractionDigits: 1,
});
const sizeNum2 = new Intl.NumberFormat('en-US', {
  minimumFractionDigits: 2,
  maximumFractionDigits: 2,
});

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${sizeNum1.format(n / 1024)} K`;
  if (n < 1024 * 1024 * 1024) return `${sizeNum1.format(n / 1048576)} M`;
  return `${sizeNum2.format(n / 1073741824)} G`;
}

/** 浮动传输指示器（批次十补强）：抽屉收起时持续可见的传输反馈。
 *  活跃期：右下角胶囊（条纹动画进度条 + 项数/字节/速率），点击打开传输中心。
 *  活跃数降到 0 的边沿：绿色「全部完成」/ 红色「有失败」小结，3s 后隐去。
 *  纯展示组件；订阅由 SftpPanel / TransferCenter 建立（无订阅源则无数据、不显示）。 */
export function TransferIndicator() {
  const t = useT();
  const bySession = useTransferStore((s) => s.bySession);
  const drawerOpen = useTransferStore((s) => s.open);
  const live = Object.values(bySession)
    .flat()
    .filter((t) => !t.history);
  const active = live.filter(
    (t) => t.state === 'queued' || t.state === 'running' || t.state === 'paused',
  );
  const activeCount = active.length;

  // 活跃 → 0 的边沿检测（渲染期派生重置，React 官方模式；两次 setState 同一拍内合并）
  const [prevCount, setPrevCount] = useState(activeCount);
  const [edge, setEdge] = useState<{ kind: 'done' | 'failed' } | null>(null);
  if (prevCount !== activeCount) {
    if (prevCount > 0 && activeCount === 0) {
      setEdge({ kind: live.some((t) => t.state === 'failed') ? 'failed' : 'done' });
    } else if (activeCount > 0 && edge !== null) {
      setEdge(null); // 新一轮传输开始，清掉上一轮的完成小结
    }
    setPrevCount(activeCount);
  }

  // 完成小结 3s 自动隐去（计时器回调里的 setState 是合法的）
  useEffect(() => {
    if (!edge) return;
    const tm = window.setTimeout(() => setEdge(null), 3000);
    return () => window.clearTimeout(tm);
  }, [edge]);

  if (drawerOpen || (activeCount === 0 && edge === null)) return null;

  const sumTotal = active.reduce((n, t) => n + t.bytesTotal, 0);
  const sumDone = active.reduce((n, t) => n + Math.min(t.bytesDone, t.bytesTotal), 0);
  const sumRate = active
    .filter((t) => t.state === 'running')
    .reduce((n, t) => n + (t.rate ?? 0), 0);
  const pct = sumTotal > 0 ? Math.min(100, (sumDone / sumTotal) * 100) : 0;

  if (edge !== null && activeCount === 0) {
    const failed = edge.kind === 'failed';
    return (
      <button
        role="status"
        className={`myssh-row-in fixed right-4 bottom-12 z-40 flex items-center gap-2 rounded-lg border px-3 py-2 text-xs shadow-xl ${
          failed
            ? 'border-red-700 bg-red-950 text-red-200'
            : 'border-green-700 bg-green-950 text-green-200'
        }`}
        onClick={() => useAppStore.getState().openDock('transfer')}
      >
        {failed ? `✕ ${t('panels.transfersFailedClick')}` : `✓ ${t('panels.allTransfersDone')}`}
      </button>
    );
  }

  return (
    <button
      role="status"
      aria-live="polite"
      title={t('panels.clickTransferManager')}
      className="myssh-row-in fixed right-4 bottom-12 z-40 flex w-64 flex-col gap-1.5 rounded-lg border border-neutral-700 bg-neutral-900 px-3 py-2 text-left text-xs text-neutral-200 shadow-xl"
      onClick={() => useAppStore.getState().openDock('transfer')}
    >
      <div className="flex items-center gap-2">
        <span className="animate-pulse text-blue-400">⇅</span>
        <span className="min-w-0 flex-1 truncate tabular-nums">
          {t('panels.transferringSummary', {
            count: activeCount,
            done: fmtSize(sumDone),
            total: fmtSize(sumTotal),
          })}
        </span>
        <span className="shrink-0 text-neutral-400 tabular-nums">{pct.toFixed(0)}%</span>
      </div>
      <div className="h-1.5 overflow-hidden rounded bg-neutral-800">
        <div
          className="myssh-progress-running h-full bg-blue-600 transition-[width] duration-300"
          style={{ width: `${pct}%` }}
        />
      </div>
      {sumRate > 0 && (
        <div className="text-right text-neutral-400 tabular-nums">{fmtSize(sumRate)}/s</div>
      )}
    </button>
  );
}
