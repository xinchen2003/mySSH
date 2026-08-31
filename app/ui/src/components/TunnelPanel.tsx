import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { ConfirmDialog } from './ConfirmDialog';
import { START_MODE_LABEL, fmtRate, startModeOf, tunnelDisplayName } from '../state/tunnel-utils';
import type { TunnelDef, TunnelInfo } from '../term/types';
import { useT, type MsgKey } from '../i18n';

const STATUS_KEY: Record<string, MsgKey> = {
  starting: 'panels.tunnelStarting',
  listening: 'panels.tunnelListening',
  reconnecting: 'panels.tunnelReconnecting',
  stopped: 'panels.tunnelStopped',
  failed: 'panels.tunnelFailed',
};

const KIND_KEY: Record<string, MsgKey> = {
  local: 'panels.tunnelKindLocal',
  remote: 'panels.tunnelKindRemote',
  dynamic: 'panels.tunnelKindDynamic',
};

/**
 * 全局隧道中心（§9.1）：按服务器分组的定义 × 1Hz 运行态合并视图。
 * 行操作：启动/停止/编辑/复制/删除；新建经 TunnelEditor（含端口预检与模板）。
 *
 * 展现形式：底部 dock 的「隧道」页签内容（原右上角弹层 TunnelPopover 已并入 dock），
 * 开关由 dock 托管（app-store dockTab）；编辑器/删除确认仍是 fixed 模态。
 */
export function TunnelPanel() {
  const t = useT();
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

  // 挂载（dock 切到隧道页签）即拉取定义；1Hz 运行态由 App 级 subscribeTunnels 提供
  useEffect(() => {
    void loadTunnelDefs();
  }, [loadTunnelDefs]);

  const runtimeById = new Map<string, TunnelInfo>(tunnels.map((t) => [t.tunnelId, t]));
  const statusLabel = (status: string) => {
    const k = STATUS_KEY[status];
    return k ? t(k) : status;
  };
  const kindLabel = (kind: string) => {
    const k = KIND_KEY[kind];
    return k ? t(k) : kind;
  };
  const defIds = new Set(tunnelDefs.map((d) => d.id));
  const adhoc = tunnels.filter((t) => !defIds.has(t.tunnelId));

  // 按服务器分组（保持会话列表顺序；孤儿定义的会话已删 → 末组）
  const grouped = new Map<string, TunnelDef[]>();
  // 隧道是 SSH 能力：新建默认目标排除本地会话
  const sshSessions = sessions.filter((s) => s.kind !== 'local');
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
      notify(t('panels.copyFailed', { error: String(e) }), 'error');
    }
  };

  const startDef = async (d: TunnelDef) => {
    try {
      await saveTunnel(d, true);
    } catch (e) {
      notify(t('panels.startFailed', { error: String(e) }), 'error');
    }
  };

  const sessionLabel = (sid: string) => {
    const s = sessions.find((x) => x.id === sid);
    if (!s) return t('panels.sessionDeleted', { id: sid });
    return s.groupPath ? `${s.groupPath} / ${s.name}` : s.name;
  };

  return (
    <div className="h-full min-h-0 overflow-y-auto px-4 py-3 text-xs text-neutral-200">
      <div className="mb-2 flex items-center gap-3 border-b border-neutral-800 pb-2 text-neutral-400">
        <span className="truncate">{t('panels.tunnelHeaderNote')}</span>
        <span className="flex-1" />
        <button
          className="shrink-0 rounded border border-neutral-700 px-2 py-0.5 text-neutral-300 hover:bg-neutral-800 focus-visible:ring-1 focus-visible:ring-neutral-500 disabled:opacity-40"
          disabled={sshSessions.length === 0}
          onClick={() =>
            sshSessions.length > 0 && setEditor({ sessionId: sshSessions[0].id, def: null })
          }
        >
          ＋ {t('panels.newTunnel')}
        </button>
      </div>

      {orderedGroups.length === 0 && adhoc.length === 0 && (
        <p className="py-2 text-neutral-400">{t('panels.noTunnels')}</p>
      )}

      {orderedGroups.map(({ sid, defs }) => {
        return (
          <div key={sid} className="mb-2">
            <div className="mt-1 mb-0.5 font-semibold text-neutral-300">{sessionLabel(sid)}</div>
            <div className="overflow-x-auto">
              <table className="w-full border-collapse whitespace-nowrap">
                <thead className="border-b border-neutral-800 text-neutral-500">
                  <tr>
                    <th className="py-0.5 pr-3 text-left font-normal">{t('panels.colName')}</th>
                    <th className="pr-3 text-left font-normal">{t('panels.colType')}</th>
                    <th className="pr-3 text-left font-normal">{t('panels.colAddress')}</th>
                    <th className="pr-3 text-left font-normal">{t('panels.colStatus')}</th>
                    <th className="pr-3 text-left font-normal">{t('panels.colStartMode')}</th>
                    <th className="pr-3 text-left font-normal">{t('panels.colRate')}</th>
                    <th className="pr-3 text-left font-normal">{t('panels.colConns')}</th>
                    <th className="pr-3 text-left font-normal">{t('panels.colError')}</th>
                    <th className="text-left font-normal">{t('panels.colActions')}</th>
                  </tr>
                </thead>
                <tbody>
                  {defs.map((d) => {
                    const rt = runtimeById.get(d.id);
                    return (
                      <tr key={d.id} className="border-t border-neutral-800/60">
                        <td className="py-1 pr-3 text-neutral-200" title={tunnelDisplayName(d)}>
                          {tunnelDisplayName(d)}
                        </td>
                        <td className="pr-3 text-neutral-400">{kindLabel(d.kind)}</td>
                        <td
                          className="pr-3 font-mono text-neutral-300"
                          title={`${d.bindHost}:${d.bindPort}${d.targetHost ? ` → ${d.targetHost}:${d.targetPort}` : ''}`}
                        >
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
                              {statusLabel(rt.status)}
                            </span>
                          ) : (
                            <span className="text-neutral-400">{t('panels.tunnelNotRunning')}</span>
                          )}
                        </td>
                        <td className="pr-3 text-neutral-400">
                          {START_MODE_LABEL[startModeOf(d)]}
                        </td>
                        <td className="pr-3 tabular-nums">
                          {rt ? `↑${fmtRate(rt.rateUp)} ↓${fmtRate(rt.rateDown)}` : '—'}
                        </td>
                        <td className="pr-3 tabular-nums">
                          {rt ? t('panels.connCount', { count: rt.activeConns }) : '—'}
                        </td>
                        <td
                          className="max-w-48 truncate pr-3 text-red-400"
                          title={rt?.lastError ?? undefined}
                        >
                          {rt?.lastError ?? ''}
                        </td>
                        <td className="whitespace-nowrap">
                          {rt ? (
                            <button
                              className="rounded px-1.5 text-neutral-400 hover:text-red-400"
                              onClick={() => void stopTunnel(d.id)}
                            >
                              {t('panels.stop')}
                            </button>
                          ) : (
                            <button
                              className="rounded px-1.5 text-neutral-400 hover:text-green-400"
                              onClick={() => void startDef(d)}
                            >
                              {t('panels.start')}
                            </button>
                          )}
                          <button
                            className="rounded px-1.5 text-neutral-400 hover:text-neutral-200"
                            onClick={() => setEditor({ sessionId: d.sessionId, def: d })}
                          >
                            {t('panels.edit')}
                          </button>
                          <button
                            className="rounded px-1.5 text-neutral-400 hover:text-neutral-200"
                            onClick={() => void duplicate(d)}
                          >
                            {t('panels.duplicate')}
                          </button>
                          <button
                            className="rounded px-1.5 text-neutral-400 hover:text-red-400"
                            onClick={() => setPendingDelete(d)}
                          >
                            {t('panels.delete')}
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>
        );
      })}

      {adhoc.length > 0 && (
        <div className="mb-1">
          <div className="mt-1 mb-0.5 font-semibold text-neutral-300">
            {t('panels.adhocTunnels')}
          </div>
          <div className="overflow-x-auto">
            <table className="w-full border-collapse whitespace-nowrap">
              <thead className="border-b border-neutral-800 text-neutral-500">
                <tr>
                  <th className="py-0.5 pr-3 text-left font-normal">{t('panels.colType')}</th>
                  <th className="pr-3 text-left font-normal">{t('panels.colAddress')}</th>
                  <th className="pr-3 text-left font-normal">{t('panels.colStatus')}</th>
                  <th className="pr-3 text-left font-normal">{t('panels.colRate')}</th>
                  <th className="text-left font-normal">{t('panels.colActions')}</th>
                </tr>
              </thead>
              <tbody>
                {adhoc.map((ti) => (
                  <tr key={ti.tunnelId} className="border-t border-neutral-800/60 text-neutral-400">
                    <td className="py-1 pr-3">{kindLabel(ti.kind)}</td>
                    <td
                      className="pr-3 font-mono text-neutral-300"
                      title={`${ti.bind}${ti.target ? ` → ${ti.target}` : ''}`}
                    >
                      {ti.bind}
                      {ti.target ? ` → ${ti.target}` : ''}
                    </td>
                    <td className="pr-3">{statusLabel(ti.status)}</td>
                    <td className="pr-3 tabular-nums">
                      ↑{fmtRate(ti.rateUp)} ↓{fmtRate(ti.rateDown)}
                    </td>
                    <td>
                      <button
                        className="rounded px-1.5 text-neutral-400 hover:text-red-400"
                        onClick={() => void stopTunnel(ti.tunnelId)}
                      >
                        {t('panels.stop')}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
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
          title={t('panels.deleteTunnelTitle', { name: tunnelDisplayName(pendingDelete) })}
          confirmLabel={t('panels.delete')}
          onConfirm={() => {
            void deleteTunnel(pendingDelete.id)
              .then(() => notify(t('panels.tunnelDeleted'), 'success'))
              .catch((e) => notify(t('panels.deleteFailed', { error: String(e) }), 'error'));
            setPendingDelete(null);
          }}
          onCancel={() => setPendingDelete(null)}
        >
          {t('panels.deleteTunnelBody', {
            addr: `${pendingDelete.bindHost}:${pendingDelete.bindPort}${pendingDelete.targetHost ? ` → ${pendingDelete.targetHost}:${pendingDelete.targetPort}` : ''}`,
          })}
        </ConfirmDialog>
      )}
    </div>
  );
}
