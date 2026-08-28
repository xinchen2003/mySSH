import { useEffect, useRef, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { ConfirmDialog } from './ConfirmDialog';
import { START_MODE_LABEL, fmtRate, startModeOf, tunnelDisplayName } from '../state/tunnel-utils';
import { usePanelWidth } from './panel-height';
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
 *
 * 展现形式（UX 批次 · 条目 7）：右上角工具条 ⇄ 触发的弹层（右锚定，左缘可拖拽调宽），
 * 不再占用底部整宽栏位。Esc / 点击外部关闭；编辑器或删除确认打开时
 * 抑制外部关闭（它们是 fixed 模态，落在弹层 DOM 之外）。
 * 开关状态复用 store 的 tunnelPanelOpen/toggleTunnelPanel，快捷键与命令面板入口不变。
 */
export function TunnelPopover({
  anchorRef,
}: {
  /** 触发按钮的容器（点击它不视为「外部」） */
  anchorRef?: React.RefObject<HTMLElement | null>;
}) {
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
  const toggleTunnelPanel = useAppStore((s) => s.toggleTunnelPanel);

  /** 编辑器目标：undefined=关闭；{sessionId, def} def=null 为新建 */
  const [editor, setEditor] = useState<{ sessionId: string; def: TunnelDef | null } | undefined>(
    undefined,
  );
  const [pendingDelete, setPendingDelete] = useState<TunnelDef | null>(null);

  const panelRef = useRef<HTMLDivElement>(null);
  // 左缘拖拽调宽（批次十 4）；弹层右锚定，向左拖变宽
  const { width, widthHandle } = usePanelWidth('ui.tunnelWidth', 560);
  /** 子模态（编辑器/删除确认）打开时抑制弹层自身的 Esc 与外部点击关闭 */
  const modalOpenRef = useRef(false);
  useEffect(() => {
    modalOpenRef.current = editor !== undefined || pendingDelete !== null;
  }, [editor, pendingDelete]);

  useEffect(() => {
    if (open) void loadTunnelDefs();
  }, [open, loadTunnelDefs]);

  // Esc / 点击外部关闭（仅在弹层打开时注册；子模态打开时静默）
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || modalOpenRef.current) return;
      e.stopPropagation();
      toggleTunnelPanel();
    };
    const onDown = (e: MouseEvent) => {
      if (modalOpenRef.current) return;
      const t = e.target as Node;
      if (panelRef.current?.contains(t)) return;
      if (anchorRef?.current?.contains(t)) return;
      toggleTunnelPanel();
    };
    window.addEventListener('keydown', onKey);
    document.addEventListener('mousedown', onDown);
    return () => {
      window.removeEventListener('keydown', onKey);
      document.removeEventListener('mousedown', onDown);
    };
  }, [open, toggleTunnelPanel, anchorRef]);

  if (!open) return null;

  const runtimeById = new Map<string, TunnelInfo>(tunnels.map((t) => [t.tunnelId, t]));
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
    <div
      ref={panelRef}
      role="dialog"
      aria-label="隧道管理"
      className="absolute top-full right-0 z-50 mt-1 max-h-[70vh] max-w-[calc(100vw-2rem)] overflow-y-auto rounded-lg border border-neutral-700 bg-neutral-900 px-4 py-3 text-xs text-neutral-200 shadow-xl"
      style={{ width }}
    >
      {widthHandle}
      <div className="mb-2 flex items-center gap-3 border-b border-neutral-800 pb-2 text-neutral-400">
        <span className="text-sm font-semibold text-neutral-100">隧道</span>
        <span className="truncate">按服务器分组 · 编辑即生效（运行中需重启）</span>
        <span className="flex-1" />
        <button
          className="shrink-0 rounded border border-neutral-700 px-2 py-0.5 text-neutral-300 hover:bg-neutral-800 disabled:opacity-40"
          disabled={sshSessions.length === 0}
          onClick={() =>
            sshSessions.length > 0 && setEditor({ sessionId: sshSessions[0].id, def: null })
          }
        >
          ＋ 新建隧道
        </button>
        <button
          className="shrink-0 rounded px-1 text-neutral-400 hover:text-neutral-200"
          onClick={toggleTunnelPanel}
          aria-label="关闭隧道面板"
        >
          ✕
        </button>
      </div>

      {orderedGroups.length === 0 && adhoc.length === 0 && (
        <p className="py-2 text-neutral-400">
          还没有隧道。点「＋ 新建隧道」，或在服务器编辑器的「隧道」页添加。
        </p>
      )}

      {orderedGroups.map(({ sid, defs }) => {
        return (
          <div key={sid} className="mb-2">
            <div className="mt-1 mb-0.5 font-semibold text-neutral-300">{sessionLabel(sid)}</div>
            <div className="overflow-x-auto">
              <table className="w-full border-collapse whitespace-nowrap">
                <thead className="border-b border-neutral-800 text-neutral-500">
                  <tr>
                    <th className="py-0.5 pr-3 text-left font-normal">名称</th>
                    <th className="pr-3 text-left font-normal">类型</th>
                    <th className="pr-3 text-left font-normal">地址</th>
                    <th className="pr-3 text-left font-normal">状态</th>
                    <th className="pr-3 text-left font-normal">启动方式</th>
                    <th className="pr-3 text-left font-normal">速率</th>
                    <th className="pr-3 text-left font-normal">连接</th>
                    <th className="pr-3 text-left font-normal">错误</th>
                    <th className="text-left font-normal">操作</th>
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
                        <td className="pr-3 text-neutral-400">{KIND_LABEL[d.kind] ?? d.kind}</td>
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
                              {STATUS_LABEL[rt.status] ?? rt.status}
                            </span>
                          ) : (
                            <span className="text-neutral-400">未运行</span>
                          )}
                        </td>
                        <td className="pr-3 text-neutral-400">
                          {START_MODE_LABEL[startModeOf(d)]}
                        </td>
                        <td className="pr-3 tabular-nums">
                          {rt ? `↑${fmtRate(rt.rateUp)} ↓${fmtRate(rt.rateDown)}` : '—'}
                        </td>
                        <td className="pr-3 tabular-nums">{rt ? `${rt.activeConns} 连接` : '—'}</td>
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
                              停止
                            </button>
                          ) : (
                            <button
                              className="rounded px-1.5 text-neutral-400 hover:text-green-400"
                              onClick={() => void startDef(d)}
                            >
                              启动
                            </button>
                          )}
                          <button
                            className="rounded px-1.5 text-neutral-400 hover:text-neutral-200"
                            onClick={() => setEditor({ sessionId: d.sessionId, def: d })}
                          >
                            编辑
                          </button>
                          <button
                            className="rounded px-1.5 text-neutral-400 hover:text-neutral-200"
                            onClick={() => void duplicate(d)}
                          >
                            复制
                          </button>
                          <button
                            className="rounded px-1.5 text-neutral-400 hover:text-red-400"
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
          </div>
        );
      })}

      {adhoc.length > 0 && (
        <div className="mb-1">
          <div className="mt-1 mb-0.5 font-semibold text-neutral-300">临时（未持久化）</div>
          <div className="overflow-x-auto">
            <table className="w-full border-collapse whitespace-nowrap">
              <thead className="border-b border-neutral-800 text-neutral-500">
                <tr>
                  <th className="py-0.5 pr-3 text-left font-normal">类型</th>
                  <th className="pr-3 text-left font-normal">地址</th>
                  <th className="pr-3 text-left font-normal">状态</th>
                  <th className="pr-3 text-left font-normal">速率</th>
                  <th className="text-left font-normal">操作</th>
                </tr>
              </thead>
              <tbody>
                {adhoc.map((t) => (
                  <tr key={t.tunnelId} className="border-t border-neutral-800/60 text-neutral-400">
                    <td className="py-1 pr-3">{KIND_LABEL[t.kind] ?? t.kind}</td>
                    <td
                      className="pr-3 font-mono text-neutral-300"
                      title={`${t.bind}${t.target ? ` → ${t.target}` : ''}`}
                    >
                      {t.bind}
                      {t.target ? ` → ${t.target}` : ''}
                    </td>
                    <td className="pr-3">{STATUS_LABEL[t.status] ?? t.status}</td>
                    <td className="pr-3 tabular-nums">
                      ↑{fmtRate(t.rateUp)} ↓{fmtRate(t.rateDown)}
                    </td>
                    <td>
                      <button
                        className="rounded px-1.5 text-neutral-400 hover:text-red-400"
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
