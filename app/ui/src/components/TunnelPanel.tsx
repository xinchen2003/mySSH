import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import type { TunnelDef, TunnelInfo, TunnelKind } from '../term/types';

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

/** 隧道面板（底部抽屉）：持久化定义与运行态（1Hz 订阅）按 id 合并 */
export function TunnelPanel() {
  const open = useAppStore((s) => s.tunnelPanelOpen);
  const tunnels = useAppStore((s) => s.tunnels);
  const tunnelDefs = useAppStore((s) => s.tunnelDefs);
  const sessions = useAppStore((s) => s.sessions);
  const stopTunnel = useAppStore((s) => s.stopTunnel);
  const saveTunnel = useAppStore((s) => s.saveTunnel);
  const deleteTunnel = useAppStore((s) => s.deleteTunnel);
  const loadTunnelDefs = useAppStore((s) => s.loadTunnelDefs);

  const [sessionId, setSessionId] = useState('');
  const [kind, setKind] = useState<TunnelKind>('local');
  const [bindHost, setBindHost] = useState('127.0.0.1');
  const [bindPort, setBindPort] = useState(1080);
  const [targetHost, setTargetHost] = useState('127.0.0.1');
  const [targetPort, setTargetPort] = useState(8080);
  const [autostart, setAutostart] = useState(false);
  const [withSession, setWithSession] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 打开面板时拉取定义（运行态由 App 级 1Hz 订阅提供）
  useEffect(() => {
    if (open) void loadTunnelDefs();
  }, [open, loadTunnelDefs]);

  if (!open) return null;

  const runtimeById = new Map<string, TunnelInfo>(tunnels.map((t) => [t.tunnelId, t]));
  // 行 = 定义 ∪ 仅运行态（ad-hoc，tn-N 序号 id 不在定义表）
  const defIds = new Set(tunnelDefs.map((d) => d.id));
  const adhoc = tunnels.filter((t) => !defIds.has(t.tunnelId));

  const submit = async () => {
    setError(null);
    try {
      if (!sessionId) throw new Error('选择会话');
      const def: TunnelDef = {
        id: `td-${Date.now()}`,
        sessionId,
        kind,
        bindHost,
        bindPort,
        targetHost: kind === 'dynamic' ? null : targetHost,
        targetPort: kind === 'dynamic' ? null : targetPort,
        autostart,
        withSession,
        createdAt: '',
      };
      // 保存即建立（运行 id = 定义 id，行内合并）
      await saveTunnel(def, true);
    } catch (e) {
      setError(String(e));
    }
  };

  const sessionName = (id: string) => sessions.find((s) => s.id === id)?.name ?? id;

  return (
    <div className="shrink-0 border-t border-neutral-800 bg-neutral-900 px-3 py-2 text-xs text-neutral-300">
      <div className="mb-1 flex items-center gap-3 text-neutral-500">
        <span className="font-semibold text-neutral-300">隧道</span>
        <span>本地 -L · 远程 -R · 动态 SOCKS5 -D · 定义持久化，独立于终端标签存活</span>
      </div>

      {(tunnelDefs.length > 0 || adhoc.length > 0) && (
        <table className="mb-2 w-full border-collapse">
          <thead>
            <tr className="text-left text-neutral-500">
              <th className="pr-3 font-normal">类型</th>
              <th className="pr-3 font-normal">会话</th>
              <th className="pr-3 font-normal">绑定</th>
              <th className="pr-3 font-normal">目标</th>
              <th className="pr-3 font-normal">状态</th>
              <th className="pr-3 font-normal">上行/下行</th>
              <th className="pr-3 font-normal">自启</th>
              <th className="pr-3 font-normal">随会话</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {tunnelDefs.map((d) => {
              const rt = runtimeById.get(d.id);
              return (
                <tr key={d.id} className="border-t border-neutral-800">
                  <td className="py-1 pr-3">{d.kind}</td>
                  <td className="pr-3">{sessionName(d.sessionId)}</td>
                  <td className="pr-3 font-mono">
                    {d.bindHost}:{d.bindPort}
                  </td>
                  <td className="pr-3 font-mono">
                    {d.targetHost ? `${d.targetHost}:${d.targetPort}` : '—'}
                  </td>
                  <td className="pr-3">{rt ? (STATUS_LABEL[rt.status] ?? rt.status) : '未运行'}</td>
                  <td className="pr-3">
                    {rt ? `↑${fmtRate(rt.rateUp)} ↓${fmtRate(rt.rateDown)}` : '—'}
                  </td>
                  <td className="pr-3">
                    <input
                      type="checkbox"
                      checked={d.autostart}
                      onChange={(e) =>
                        void saveTunnel({ ...d, autostart: e.target.checked }, false)
                      }
                    />
                  </td>
                  <td className="pr-3">
                    <input
                      type="checkbox"
                      checked={d.withSession}
                      onChange={(e) =>
                        void saveTunnel({ ...d, withSession: e.target.checked }, false)
                      }
                    />
                  </td>
                  <td className="whitespace-nowrap">
                    {rt ? (
                      <button
                        className="rounded px-1.5 text-neutral-500 hover:text-red-400"
                        onClick={() => void stopTunnel(d.id)}
                      >
                        停止
                      </button>
                    ) : (
                      <button
                        className="rounded px-1.5 text-neutral-500 hover:text-green-400"
                        onClick={() => void saveTunnel(d, true)}
                      >
                        启动
                      </button>
                    )}
                    <button
                      className="rounded px-1.5 text-neutral-500 hover:text-red-400"
                      onClick={() => void deleteTunnel(d.id)}
                    >
                      删除
                    </button>
                  </td>
                </tr>
              );
            })}
            {adhoc.map((t) => (
              <tr key={t.tunnelId} className="border-t border-neutral-800 text-neutral-500">
                <td className="py-1 pr-3">{t.kind}</td>
                <td className="pr-3">（临时）</td>
                <td className="pr-3 font-mono">{t.bind}</td>
                <td className="pr-3 font-mono">{t.target ?? '—'}</td>
                <td className="pr-3">{STATUS_LABEL[t.status] ?? t.status}</td>
                <td className="pr-3">
                  ↑{fmtRate(t.rateUp)} ↓{fmtRate(t.rateDown)}
                </td>
                <td></td>
                <td></td>
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
        <label className="flex items-center gap-1 text-neutral-400" title="app 启动即建立">
          <input
            type="checkbox"
            checked={autostart}
            onChange={(e) => setAutostart(e.target.checked)}
          />
          自启
        </label>
        <label
          className="flex items-center gap-1 text-neutral-400"
          title="该会话终端连接成功后自动建立"
        >
          <input
            type="checkbox"
            checked={withSession}
            onChange={(e) => setWithSession(e.target.checked)}
          />
          随会话
        </label>
        <button
          className="rounded bg-blue-600 px-2.5 py-1 text-white hover:bg-blue-500"
          onClick={() => void submit()}
        >
          保存并启动
        </button>
        {error && <span className="text-red-400">{error}</span>}
      </div>
    </div>
  );
}
