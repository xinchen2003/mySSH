import { useEffect, useRef, useState } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import type { MetricsEvent, MetricsSnapshot } from '../term/types';
import { usePanelHeight } from './panel-height';

/** 环形缓冲长度（2s 间隔 ≈ 4 分钟窗口） */
const CAP = 120;

interface Series {
  label: string;
  color: string;
  /** 取值器：None → 断点（首轮无差分） */
  get: (s: MetricsSnapshot) => number | null;
  /** 单位格式化（纵轴峰值 + 当前值） */
  fmt: (v: number) => string;
  /** 固定纵轴上限（如百分比 100）；否则自适应峰值 */
  max?: number;
}

const fmtBps = (v: number): string => {
  if (v >= 1 << 30) return `${(v / (1 << 30)).toFixed(1)}G`;
  if (v >= 1 << 20) return `${(v / (1 << 20)).toFixed(1)}M`;
  if (v >= 1024) return `${(v / 1024).toFixed(0)}K`;
  return `${v.toFixed(0)}`;
};

const sumOpt = (xs: (number | null | undefined)[]): number | null =>
  xs.some((x) => x == null) ? null : (xs as number[]).reduce((a, b) => a + b, 0);

const CHARTS: { title: string; series: Series[] }[] = [
  {
    title: 'CPU / 内存 %',
    series: [
      {
        label: 'cpu',
        color: '#4ade80',
        get: (s) => s.cpuBusyPct ?? null,
        fmt: (v) => `${v.toFixed(0)}%`,
        max: 100,
      },
      {
        label: 'mem',
        color: '#60a5fa',
        get: (s) =>
          s.memTotalKb > 0 ? (100 * (s.memTotalKb - s.memAvailKb)) / s.memTotalKb : null,
        fmt: (v) => `${v.toFixed(0)}%`,
        max: 100,
      },
    ],
  },
  {
    title: '网络 IO',
    series: [
      {
        label: 'rx',
        color: '#4ade80',
        get: (s) => sumOpt(s.nets.map((n) => n.rxBps)),
        fmt: (v) => `${fmtBps(v)}/s`,
      },
      {
        label: 'tx',
        color: '#f472b6',
        get: (s) => sumOpt(s.nets.map((n) => n.txBps)),
        fmt: (v) => `${fmtBps(v)}/s`,
      },
    ],
  },
  {
    title: '磁盘 IO',
    series: [
      {
        label: 'read',
        color: '#4ade80',
        get: (s) => sumOpt(s.disks.map((d) => d.readBps)),
        fmt: (v) => `${fmtBps(v)}/s`,
      },
      {
        label: 'write',
        color: '#f59e0b',
        get: (s) => sumOpt(s.disks.map((d) => d.writeBps)),
        fmt: (v) => `${fmtBps(v)}/s`,
      },
    ],
  },
];

function Chart({
  title,
  series,
  buf,
}: {
  title: string;
  series: Series[];
  buf: MetricsSnapshot[];
}) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const cv = ref.current;
    if (!cv) return;
    const w = cv.clientWidth;
    const h = cv.clientHeight;
    const dpr = window.devicePixelRatio || 1;
    cv.width = w * dpr;
    cv.height = h * dpr;
    const g = cv.getContext('2d');
    if (!g) return;
    g.scale(dpr, dpr);
    g.clearRect(0, 0, w, h);
    // 纵轴上限：固定 max 或全序列自适应峰值
    const autoMax = Math.max(1, ...buf.flatMap((s) => series.map((sr) => sr.get(s) ?? 0)));
    const peak = series[0]?.max ?? autoMax;
    const label = series
      .map((sr) => {
        const v = buf.length > 0 ? sr.get(buf[buf.length - 1]) : null;
        return v == null ? `${sr.label} -` : `${sr.label} ${sr.fmt(v)}`;
      })
      .join('  ');
    g.fillStyle = '#a3a3a3';
    g.font = '10px sans-serif';
    g.fillText(`${title}   ${label}   [峰值 ${series[0]?.fmt(peak) ?? ''}]`, 6, 11);
    const top = 14;
    const plotH = h - top - 2;
    for (const sr of series) {
      g.strokeStyle = sr.color;
      g.lineWidth = 1;
      g.beginPath();
      let started = false;
      buf.forEach((s, i) => {
        const v = sr.get(s);
        if (v == null) {
          started = false;
          return;
        }
        const x = (i / (CAP - 1)) * w;
        const y = top + plotH * (1 - Math.min(v / peak, 1));
        if (started) g.lineTo(x, y);
        else g.moveTo(x, y);
        started = true;
      });
      g.stroke();
    }
  });
  return <canvas ref={ref} className="h-16 w-full" />;
}

export function MetricsPanel({ tabId }: { tabId: string }) {
  const tabs = useAppStore((s) => s.tabs);
  const toggleMetrics = useAppStore((s) => s.toggleMetrics);
  const tab = tabs.find((t) => t.id === tabId);
  const sessions = useAppStore((s) => s.sessions);
  const rawSessionId = tab?.target.kind === 'session' ? tab.target.sessionId : null;
  // 本地会话无远程主机可监控：按无会话处理（return null）
  const sessionId =
    sessions.find((r) => r.id === rawSessionId)?.kind === 'local' ? null : rawSessionId;

  const [buf, setBuf] = useState<MetricsSnapshot[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [fatal, setFatal] = useState(false);
  const [intervalMs, setIntervalMs] = useState(2000);

  useEffect(() => {
    if (!sessionId) return;
    queueMicrotask(() => {
      setBuf([]);
      setError(null);
      setFatal(false);
    });
    const events = new Channel<MetricsEvent>();
    events.onmessage = (ev) => {
      if (ev.kind === 'snapshot') {
        setBuf((prev) => [...prev.slice(-(CAP - 1)), ev.data]);
      } else {
        setError(ev.message);
        if (ev.fatal) setFatal(true);
      }
    };
    void invoke('metrics_subscribe', { sessionId, intervalMs, events }).catch((e) =>
      setError(String(e)),
    );
    return () => {
      void invoke('metrics_unsubscribe', { sessionId }).catch(() => undefined);
    };
  }, [sessionId, intervalMs]);

  const { height, handle } = usePanelHeight('metrics.height', 256);
  if (!sessionId) return null;
  const latest = buf.length > 0 ? buf[buf.length - 1] : null;
  const memUsedGb = latest ? (latest.memTotalKb - latest.memAvailKb) / 1048576 : 0;
  const memTotalGb = latest ? latest.memTotalKb / 1048576 : 0;
  const swapUsedGb = latest ? (latest.swapTotalKb - latest.swapFreeKb) / 1048576 : 0;

  return (
    <div
      className="flex shrink-0 flex-col border-t border-neutral-700 bg-neutral-900 text-xs text-neutral-300"
      style={{ height }}
    >
      {handle}
      <div className="flex items-center gap-3 border-b border-neutral-800 px-2 py-1">
        <span className="font-semibold text-neutral-200">监控</span>
        {latest && (
          <span className="text-neutral-400">
            load {latest.load.map((v) => v.toFixed(2)).join(' / ')} · 内存 {memUsedGb.toFixed(1)}/
            {memTotalGb.toFixed(1)}G{latest.swapTotalKb > 0 && ` · swap ${swapUsedGb.toFixed(1)}G`}{' '}
            · 进程 {latest.procsRunning}/{latest.procsTotal}
          </span>
        )}
        {error && (
          <span className={fatal ? 'text-red-400' : 'text-amber-400'}>
            {fatal ? '采集不可用' : '采集异常'}: {error}
          </span>
        )}
        <span className="ml-auto flex items-center gap-2">
          <select
            className="rounded bg-neutral-800 px-1 py-0.5"
            value={intervalMs}
            onChange={(e) => setIntervalMs(Number(e.target.value))}
            title="采集间隔"
          >
            <option value={2000}>2s</option>
            <option value={5000}>5s</option>
            <option value={10000}>10s</option>
          </select>
          <button
            className="rounded px-1 text-neutral-500 hover:text-neutral-200"
            onClick={() => toggleMetrics(tabId)}
            aria-label="close metrics"
          >
            ✕
          </button>
        </span>
      </div>
      <div className="grid flex-1 grid-cols-3 gap-1 overflow-hidden px-1">
        {CHARTS.map((c) => (
          <Chart key={c.title} title={c.title} series={c.series} buf={buf} />
        ))}
      </div>
      <div className="h-24 overflow-y-auto border-t border-neutral-800">
        <table className="w-full text-left">
          <thead className="sticky top-0 bg-neutral-900 text-neutral-500">
            <tr>
              <th className="px-2 font-normal">PID</th>
              <th className="px-2 font-normal">RSS</th>
              <th className="px-2 font-normal">CPU%</th>
              <th className="px-2 font-normal">MEM%</th>
              <th className="px-2 font-normal">进程</th>
            </tr>
          </thead>
          <tbody>
            {(latest?.procs ?? []).slice(0, 15).map((p) => (
              <tr key={p.pid} className="border-t border-neutral-800/50">
                <td className="px-2">{p.pid}</td>
                <td className="px-2">{fmtBps(p.rssKb * 1024)}</td>
                <td className="px-2">{p.cpuPct.toFixed(1)}</td>
                <td className="px-2">{p.memPct.toFixed(1)}</td>
                <td className="truncate px-2" title={p.comm}>
                  {p.comm}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}
