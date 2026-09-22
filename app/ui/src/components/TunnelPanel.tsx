import { useEffect, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { ConfirmDialog } from './ConfirmDialog';
import {
  START_MODE_LABEL,
  TUNNEL_KIND_KEY,
  TUNNEL_STATUS_KEY,
  fmtRate,
  startModeOf,
  tunnelDisplayName,
} from '../state/tunnel-utils';
import type { TunnelDef, TunnelInfo } from '../term/types';
import { useT } from '../i18n';

/**
 * 隧道面板（§9.1）：当前活动会话的隧道定义 × 1Hz 运行态合并视图。
 * 行操作：启动/停止/编辑/复制/删除；新建经 TunnelEditor（含端口预检与模板）。
 *
 * 只显示活动页签关联会话的隧道（隧道固定归属会话，跨会话全量列表无操作意义）。
 * 运行态无 sessionId 字段的临时（非持久化）隧道无法归属，批次二十九起已无
 * 产生路径（一切隧道必经定义），旧 adhoc 区块随之移除。
 *
 * 展现形式：底部 dock 的「隧道」页签内容（原右上角弹层 TunnelPopover 已并入 dock），
 * 开关由 dock 托管（app-store dockTab）；编辑器/删除确认仍是 fixed 模态。
 */
export function TunnelPanel() {
  const t = useT();
  const tunnels = useAppStore((s) => s.tunnels);
  const tunnelDefs = useAppStore((s) => s.tunnelDefs);
  const activeId = useAppStore((s) => s.activeId);
  const tabs = useAppStore((s) => s.tabs);
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

  // 活动页签关联的会话 id（快速连接等 spec 目标无档案 → null）
  const activeTab = tabs.find((t2) => t2.id === activeId);
  const sessionId = activeTab?.target.kind === 'session' ? activeTab.target.sessionId : null;
  const defs = sessionId ? tunnelDefs.filter((d) => d.sessionId === sessionId) : [];

  const runtimeById = new Map<string, TunnelInfo>(tunnels.map((t) => [t.tunnelId, t]));
  const statusLabel = (status: string) => {
    const k = TUNNEL_STATUS_KEY[status];
    return k ? t(k) : status;
  };
  const kindLabel = (kind: string) => {
    const k = TUNNEL_KIND_KEY[kind];
    return k ? t(k) : kind;
  };

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

  return (
    <div className="h-full min-h-0 overflow-y-auto px-4 py-3 text-xs text-neutral-200">
      <div className="mb-2 flex items-center gap-3 border-b border-neutral-800 pb-2 text-neutral-400">
        <span className="truncate">{t('panels.tunnelHeaderNote')}</span>
        <span className="flex-1" />
        {sessionId && (
          <button
            className="shrink-0 rounded border border-neutral-700 px-2 py-0.5 text-neutral-300 hover:bg-neutral-800"
            onClick={() => setEditor({ sessionId, def: null })}
          >
            {t('panels.newTunnel')}
          </button>
        )}
      </div>

      {!sessionId ? (
        <p className="py-2 text-neutral-400">{t('panels.tunnelNoActiveSession')}</p>
      ) : defs.length === 0 ? (
        <p className="py-2 text-neutral-400">{t('panels.noTunnels')}</p>
      ) : (
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
                <th className="pr-3 text-left font-normal" title={t('panels.colConnsHint')}>
                  {t('panels.colConns')}
                </th>
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
                    <td className="pr-3 text-neutral-400">{START_MODE_LABEL[startModeOf(d)]}</td>
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
