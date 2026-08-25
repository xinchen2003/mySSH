import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { ConfirmDialog } from './ConfirmDialog';
import { START_MODE_LABEL, fmtRate, startModeOf, tunnelDisplayName } from '../state/tunnel-utils';
import type { TunnelDef, TunnelInfo } from '../term/types';

const STATUS_LABEL: Record<string, string> = {
  starting: '启动中',
  listening: '监听中',
  reconnecting: '重连中',
  stopped: '已停止',
  failed: '失败',
};

const KIND_LABEL: Record<string, string> = {
  local: '本地 -L',
  remote: '远程 -R',
  dynamic: 'SOCKS5 -D',
};

/**
 * 全局隧道中心（§9.1 双入口之二）：按服务器分组的定义 × 1Hz 运行态合并视图。
 * 行操作：启动/停止/编辑/复制/删除；新建经 TunnelEditor（含端口预检与模板）。
 */
export function TunnelPanel() {
  const open = useAppStore((s) => s.tunnelPanelOpen);
  const tunnels = useAppStore((s) => s.tunnels);
  const tunnelDefs = useAppStore((s) => s.tunnelDefs);
  const sessions = useAppStore((s) => s.sessions);
  const stopTunnel = useAppStore((s) => s.stopTunnel);
  const saveTunnel = useAppStore((s) => s.saveTunnel);
  const deleteTunnel = useAppStore((s) => s.deleteTunnel);
  const loadTunnelDefs = useAppStore((s) => s.loadTunnelDefs);
  const notify = useAppStore((s) => s.notify);
  const duplicateTunnel = useAppStore((s) => s.duplicateTunnel);

  /** 编辑器目标：undefined=关闭；{sessionId, def} def=null 为新建 */
  const [editor, setEditor] = useState<{ sessionId: string; def: TunnelDef | null } | undefined>(
    undefined,
  );
  const [pendingDelete, setPendingDelete] = useState<TunnelDef | null>(null);

  useEffect(() => {
    if (open) void loadTunnelDefs();
  }, [open, loadTunnelDefs]);

  if (!open) return null;

  const runtimeById = new Map<string, TunnelInfo>(tunnels.map((t) => [t.tunnelId, t]));
  const defIds = new Set(tunnelDefs.map((d) => d.id));
  const adhoc = tunnels.filter((t) => !defIds.has(t.tunnelId));

  // 按服务器分组（保持会话列表顺序；孤儿定义的会话已删 → 末组）
  const grouped = new Map<string, TunnelDef[]>();
  for (const d of tunnelDefs) {
    const list = grouped.get(d.sessionId) ?? [];
    list.push(d);
    grouped.set(d.sessionId, list);
  }
  const orderedGroups: { sid: string; defs: TunnelDef[] }[] = [
    ...sessions.flatMap((s) => {
      const defs = grouped.get(s.id);
      return defs ? [{ sid: s.id, defs }] : [];
    }),
    ...[...grouped.entries()]
      .filter(([sid]) => !sessions.some((s) => s.id === sid))
      .map(([sid, defs]) => ({ sid, defs })),
  ];

  const duplicate = async (d: TunnelDef) => {
    try {
      await duplicateTunnel(d);
    } catch (e) {
      notify(`复制失败: ${String(e)}`, 'error');
    }
  };

  const startDef = async (d: TunnelDef) => {
    try {
      await saveTunnel(d, true);
    } catch (e) {
      notify(`启动失败: ${String(e)}`, 'error');
    }
  };

  const sessionLabel = (sid: string) => {
    const s = sessions.find((x) => x.id === sid);
    if (!s) return `（会话已删除 ${sid}）`;
    return s.groupPath ? `${s.groupPath} / ${s.name}` : s.name;
  };

  return (
    <div className="max-h-72 shrink-0 overflow-y-auto border-t border-neutral-800 bg-neutral-900 px-3 py-2 text-xs text-neutral-300">
      <div className="mb-1 flex items-center gap-3 text-neutral-500">
        <span className="font-semibold text-neutral-300">隧道</span>
        <span>按服务器分组 · 编辑即生效（运行中需重启）· 独立于终端标签存活</span>
        <span className="flex-1" />
        <button
          className="rounded border border-neutral-700 px-2 py-0.5 hover:bg-neutral-800 disabled:opacity-40"
          disabled={sessions.length === 0}
          onClick={() => sessions.length > 0 && setEditor({ sessionId: sessions[0].id, def: null })}
        >
          ＋ 新建隧道
        </button>
      </div>

      {orderedGroups.length === 0 && adhoc.length === 0 && (
        <p className="py-2 text-neutral-500">
          还没有隧道。点「＋ 新建隧道」，或在服务器编辑器的「隧道」页添加。
        </p>
      )}

      {orderedGroups.map(({ sid, defs }) => {
        return (
          <div key={sid} className="mb-2">
            <div className="mt-1 mb-0.5 font-semibold text-neutral-400">{sessionLabel(sid)}</div>
            <table className="w-full border-collapse">
              <tbody>
                {defs.map((d) => {
                  const rt = runtimeById.get(d.id);
                  return (
                    <tr key={d.id} className="border-t border-neutral-800/60">
                      <td className="py-1 pr-3">{tunnelDisplayName(d)}</td>
                      <td className="pr-3 text-neutral-500">{KIND_LABEL[d.kind] ?? d.kind}</td>
                      <td className="pr-3 font-mono">
                        {d.bindHost}:{d.bindPort}
                        {d.targetHost ? ` → ${d.targetHost}:${d.targetPort}` : ''}
                      </td>
                      <td className="pr-3">
                        {rt ? (
                          <span
                            className={
                              rt.status === 'listening'
                                ? 'text-green-400'
                                : rt.status === 'failed'
                                  ? 'text-red-400'
                                  : 'text-yellow-400'
                            }
                          >
                            {STATUS_LABEL[rt.status] ?? rt.status}
                          </span>
                        ) : (
                          <span className="text-neutral-600">未运行</span>
                        )}
                      </td>
                      <td className="pr-3 text-neutral-500">{START_MODE_LABEL[startModeOf(d)]}</td>
                      <td className="pr-3">
                        {rt ? `↑${fmtRate(rt.rateUp)} ↓${fmtRate(rt.rateDown)}` : '—'}
                      </td>
                      <td className="pr-3">{rt ? `${rt.activeConns} 连接` : '—'}</td>
                      <td
                        className="max-w-48 truncate pr-3 text-red-400/80"
                        title={rt?.lastError ?? undefined}
                      >
                        {rt?.lastError ?? ''}
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
                            onClick={() => void startDef(d)}
                          >
                            启动
                          </button>
                        )}
                        <button
                          className="rounded px-1.5 text-neutral-500 hover:text-neutral-200"
                          onClick={() => setEditor({ sessionId: d.sessionId, def: d })}
                        >
                          编辑
                        </button>
                        <button
                          className="rounded px-1.5 text-neutral-500 hover:text-neutral-200"
                          onClick={() => void duplicate(d)}
                        >
                          复制
                        </button>
                        <button
                          className="rounded px-1.5 text-neutral-500 hover:text-red-400"
                          onClick={() => setPendingDelete(d)}
                        >
                          删除
                        </button>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        );
      })}

      {adhoc.length > 0 && (
        <div className="mb-1">
          <div className="mt-1 mb-0.5 font-semibold text-neutral-500">临时（未持久化）</div>
          <table className="w-full border-collapse">
            <tbody>
              {adhoc.map((t) => (
                <tr key={t.tunnelId} className="border-t border-neutral-800/60 text-neutral-500">
                  <td className="py-1 pr-3">{KIND_LABEL[t.kind] ?? t.kind}</td>
                  <td className="pr-3 font-mono">
                    {t.bind}
                    {t.target ? ` → ${t.target}` : ''}
                  </td>
                  <td className="pr-3">{STATUS_LABEL[t.status] ?? t.status}</td>
                  <td className="pr-3">
                    ↑{fmtRate(t.rateUp)} ↓{fmtRate(t.rateDown)}
                  </td>
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
        </div>
      )}

      {editor && (
        <TunnelEditor
          sessionId={editor.sessionId}
          initial={editor.def}
          running={editor.def ? runtimeById.has(editor.def.id) : false}
          onClose={() => setEditor(undefined)}
        />
      )}
      {pendingDelete && (
        <ConfirmDialog
          title={`删除隧道「${tunnelDisplayName(pendingDelete)}」？`}
          confirmLabel="删除"
          onConfirm={() => {
            void deleteTunnel(pendingDelete.id)
              .then(() => notify('隧道已删除', 'success'))
              .catch((e) => notify(`删除失败: ${String(e)}`, 'error'));
            setPendingDelete(null);
          }}
          onCancel={() => setPendingDelete(null)}
        >
          {pendingDelete.bindHost}:{pendingDelete.bindPort}
          {pendingDelete.targetHost
            ? ` → ${pendingDelete.targetHost}:${pendingDelete.targetPort}`
            : ''}
          。运行中的实例将同时停止。
        </ConfirmDialog>
      )}
    </div>
  );
}
