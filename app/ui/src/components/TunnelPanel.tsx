import { useState } from 'react';
import { useAppStore } from '../state/app-store';
import type { TunnelKind } from '../term/types';

const STATUS_LABEL: Record<string, string> = {
  starting: '启动中',
  listening: '监听中',
  reconnecting: '重连中',
  stopped: '已停止',
  failed: '失败',
};

function fmtRate(bytesPerSec: number): string {
  if (bytesPerSec >= 1 << 20) return `${(bytesPerSec / (1 << 20)).toFixed(1)} MB/s`;
  if (bytesPerSec >= 1024) return `${(bytesPerSec / 1024).toFixed(1)} KB/s`;
  return `${bytesPerSec} B/s`;
}

/** 隧道面板（底部抽屉）：列表 1Hz 刷新 + 新建表单 */
export function TunnelPanel() {
  const open = useAppStore((s) => s.tunnelPanelOpen);
  const tunnels = useAppStore((s) => s.tunnels);
  const sessions = useAppStore((s) => s.sessions);
  const stopTunnel = useAppStore((s) => s.stopTunnel);
  const startTunnel = useAppStore((s) => s.startTunnel);

  const [sessionId, setSessionId] = useState('');
  const [kind, setKind] = useState<TunnelKind>('local');
  const [bindHost, setBindHost] = useState('127.0.0.1');
  const [bindPort, setBindPort] = useState(1080);
  const [targetHost, setTargetHost] = useState('127.0.0.1');
  const [targetPort, setTargetPort] = useState(8080);
  const [error, setError] = useState<string | null>(null);

  if (!open) return null;

  const submit = async () => {
    setError(null);
    try {
      if (!sessionId) throw new Error('选择会话');
      await startTunnel({
        sessionId,
        kind,
        bindHost,
        bindPort,
        targetHost: kind === 'dynamic' ? undefined : targetHost,
        targetPort: kind === 'dynamic' ? undefined : targetPort,
      });
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <div className="shrink-0 border-t border-neutral-800 bg-neutral-900 px-3 py-2 text-xs text-neutral-300">
      <div className="mb-1 flex items-center gap-3 text-neutral-500">
        <span className="font-semibold text-neutral-300">隧道</span>
        <span>本地 -L · 远程 -R · 动态 SOCKS5 -D</span>
      </div>

      {tunnels.length > 0 && (
        <table className="mb-2 w-full border-collapse">
          <thead>
            <tr className="text-left text-neutral-500">
              <th className="pr-3 font-normal">类型</th>
              <th className="pr-3 font-normal">绑定</th>
              <th className="pr-3 font-normal">目标</th>
              <th className="pr-3 font-normal">状态</th>
              <th className="pr-3 font-normal">连接</th>
              <th className="pr-3 font-normal">上行/下行</th>
              <th className="font-normal">重连</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {tunnels.map((t) => (
              <tr key={t.tunnelId} className="border-t border-neutral-800">
                <td className="pr-3 py-1">{t.kind}</td>
                <td className="pr-3 font-mono">{t.bind}</td>
                <td className="pr-3 font-mono">{t.target ?? '—'}</td>
                <td className="pr-3">{STATUS_LABEL[t.status] ?? t.status}</td>
                <td className="pr-3">
                  {t.activeConns}/{t.totalConns}
                </td>
                <td className="pr-3">
                  ↑{fmtRate(t.rateUp)} ↓{fmtRate(t.rateDown)}
                </td>
                <td className="pr-3">{t.reconnects}</td>
                <td>
                  <button
                    className="rounded px-1.5 text-neutral-500 hover:text-red-400"
                    onClick={() => void stopTunnel(t.tunnelId)}
                  >
                    停止
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <div className="flex items-center gap-2">
        <select
          className="rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1"
          value={sessionId}
          onChange={(e) => setSessionId(e.target.value)}
        >
          <option value="">选择会话…</option>
          {sessions.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </select>
        <select
          className="rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1"
          value={kind}
          onChange={(e) => setKind(e.target.value as TunnelKind)}
        >
          <option value="local">本地 -L</option>
          <option value="remote">远程 -R</option>
          <option value="dynamic">动态 -D</option>
        </select>
        <input
          className="w-24 rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1"
          value={bindHost}
          onChange={(e) => setBindHost(e.target.value)}
          title="绑定地址"
        />
        <input
          className="w-16 rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1"
          type="number"
          value={bindPort}
          onChange={(e) => setBindPort(Number(e.target.value))}
          title="绑定端口"
        />
        {kind !== 'dynamic' && (
          <>
            <span className="text-neutral-600">→</span>
            <input
              className="w-24 rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1"
              value={targetHost}
              onChange={(e) => setTargetHost(e.target.value)}
              title="目标地址"
            />
            <input
              className="w-16 rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1"
              type="number"
              value={targetPort}
              onChange={(e) => setTargetPort(Number(e.target.value))}
              title="目标端口"
            />
          </>
        )}
        <button
          className="rounded bg-blue-600 px-2.5 py-1 text-white hover:bg-blue-500"
          onClick={() => void submit()}
        >
          启动
        </button>
        {error && <span className="text-red-400">{error}</span>}
      </div>
    </div>
  );
}
