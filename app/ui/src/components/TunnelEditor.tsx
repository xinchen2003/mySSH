import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import { ConfirmDialog } from './ConfirmDialog';
import { Dialog } from './Dialog';
import {
  TUNNEL_TEMPLATES,
  START_MODE_LABEL,
  draftFromDef,
  draftToDef,
  validateTunnelDraft,
  isWildcardBind,
  type TunnelDraft,
} from '../state/tunnel-utils';
import type { TunnelDef, TunnelKind, TunnelStartMode } from '../term/types';
import { useT } from '../i18n';

interface PortCheck {
  available: boolean;
  selfOccupied?: boolean;
  holder?: string;
  suggestedPort?: number | null;
}

/**
 * 隧道编辑器（§9.1 服务器编辑器 / §9.1 全局面板共用）。
 *
 * 语义：
 * - 编辑走 upsert（id 保留，单语句原子），绝不删除再创建；
 * - 保存运行中的隧道后提示是否重启以应用新配置；
 * - 本地/动态隧道保存前做端口预检（§9.4），冲突时给「使用建议端口/重新检测/取消」，
 *   绝不静默修改用户端口；
 * - 模板只预填字段，不隐藏实际配置（§9.5）。
 */
export function TunnelEditor({
  sessionId,
  initial,
  running,
  onClose,
}: {
  sessionId: string;
  /** null = 新建 */
  initial: TunnelDef | null;
  /** 编辑对象当前是否在运行（决定是否提示重启） */
  running: boolean;
  onClose: () => void;
}) {
  const saveTunnel = useAppStore((s) => s.saveTunnel);
  const stopTunnel = useAppStore((s) => s.stopTunnel);
  const notify = useAppStore((s) => s.notify);
  const t = useT();

  const [draft, setDraft] = useState<TunnelDraft>(() =>
    initial
      ? draftFromDef(initial)
      : {
          id: `td-${crypto.randomUUID()}`,
          sessionId,
          name: '',
          kind: 'local',
          bindHost: '127.0.0.1',
          bindPort: '',
          targetHost: '127.0.0.1',
          targetPort: '',
          startMode: 'withSession',
        },
  );
  const [error, setError] = useState<string | null>(null);
  const [conflict, setConflict] = useState<PortCheck | null>(null);
  const [pendingRestart, setPendingRestart] = useState<TunnelDef | null>(null);
  const [busy, setBusy] = useState(false);

  const patch = (p: Partial<TunnelDraft>) => {
    setDraft((d) => ({ ...d, ...p }));
    setError(null);
    setConflict(null);
  };

  const applyTemplate = (key: string) => {
    const t = TUNNEL_TEMPLATES[key];
    if (!t) return;
    patch({
      name: t.label,
      kind: t.kind,
      bindPort: String(t.bindPort),
      targetHost: t.targetHost ?? '',
      targetPort: t.targetPort ? String(t.targetPort) : '',
    });
  };

  const persist = async (d: TunnelDraft): Promise<TunnelDef | null> => {
    const def = draftToDef(d, initial?.createdAt ?? '');
    await saveTunnel(def, false);
    return def;
  };

  const save = async (override?: TunnelDraft) => {
    setError(null);
    const current = override ?? draft;
    const err = validateTunnelDraft(current);
    if (err) {
      setError(err);
      return;
    }
    setBusy(true);
    try {
      // 端口预检：仅本地监听类（local/dynamic）；remote 无本地绑定
      if (current.kind !== 'remote') {
        const check = await invoke<PortCheck>('tunnel_check_port', {
          host: current.bindHost.trim(),
          port: Number(current.bindPort),
          excludeTunnelId: initial?.id ?? null,
        });
        if (!check.available) {
          setConflict(check);
          return;
        }
      }
      const def = await persist(current);
      if (!def) return;
      if (initial && running) {
        setPendingRestart(def);
        return;
      }
      notify(initial ? t('dialogs.tunnelSaved') : t('dialogs.tunnelCreated'), 'success');
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const restartNow = async () => {
    if (!pendingRestart) return;
    setBusy(true);
    try {
      await stopTunnel(pendingRestart.id);
      await saveTunnel(pendingRestart, true);
      notify(t('dialogs.tunnelRestarted'), 'success');
      onClose();
    } catch (e) {
      setPendingRestart(null);
      setError(t('dialogs.tunnelRestartFailed', { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const input =
    'w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1 text-sm text-neutral-200';

  return (
    <Dialog
      title={initial ? t('dialogs.editTunnel') : t('dialogs.newTunnel')}
      onClose={onClose}
      backdropClass="z-40"
      panelClass="w-[26rem] rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
    >
      <h2 className="mb-3 text-base font-semibold text-neutral-100">
        {initial ? t('dialogs.editTunnel') : t('dialogs.newTunnel')}
      </h2>

      <label className="mb-2 block">
        <span className="mb-0.5 block text-xs text-neutral-400">{t('dialogs.templateLabel')}</span>
        <select
          className={input}
          value=""
          onChange={(e) => applyTemplate(e.target.value)}
          aria-label={t('dialogs.templateAria')}
        >
          <option value="">{t('dialogs.templateCustom')}</option>
          {Object.entries(TUNNEL_TEMPLATES).map(([k, t]) => (
            <option key={k} value={k}>
              {t.label}
            </option>
          ))}
        </select>
      </label>

      <div className="mb-2 flex gap-2">
        <label className="flex-1">
          <span className="mb-0.5 block text-xs text-neutral-400">{t('dialogs.labelName')}</span>
          <input
            className={input}
            value={draft.name}
            onChange={(e) => patch({ name: e.target.value })}
            placeholder={t('dialogs.tunnelNamePlaceholder')}
          />
        </label>
        <label className="w-28">
          <span className="mb-0.5 block text-xs text-neutral-400">{t('dialogs.typeLabel')}</span>
          <select
            className={input}
            value={draft.kind}
            onChange={(e) => patch({ kind: e.target.value as TunnelKind })}
            aria-label={t('dialogs.typeLabel')}
          >
            <option value="local">{t('dialogs.tunnelKindLocal')}</option>
            <option value="remote">{t('dialogs.tunnelKindRemote')}</option>
            <option value="dynamic">{t('dialogs.tunnelKindDynamic')}</option>
          </select>
        </label>
      </div>

      <div className="mb-2 flex items-end gap-2">
        <label className="flex-1">
          <span className="mb-0.5 block text-xs text-neutral-400">{t('dialogs.bindAddress')}</span>
          <input
            className={input}
            spellCheck={false}
            autoComplete="off"
            value={draft.bindHost}
            onChange={(e) => patch({ bindHost: e.target.value })}
          />
        </label>
        <label className="w-24">
          <span className="mb-0.5 block text-xs text-neutral-400">{t('dialogs.bindPort')}</span>
          <input
            className={input}
            type="number"
            min={1}
            max={65535}
            value={draft.bindPort}
            onChange={(e) => patch({ bindPort: e.target.value })}
          />
        </label>
      </div>
      {isWildcardBind(draft.bindHost) && (
        <p className="mb-2 rounded border border-yellow-800 bg-yellow-950/40 px-2 py-1 text-xs text-yellow-300">
          {t('dialogs.wildcardWarning')}
        </p>
      )}

      {draft.kind !== 'dynamic' && (
        <div className="mb-2 flex items-end gap-2">
          <label className="flex-1">
            <span className="mb-0.5 block text-xs text-neutral-400">
              {t('dialogs.targetAddress')}
            </span>
            <input
              className={input}
              spellCheck={false}
              autoComplete="off"
              value={draft.targetHost}
              onChange={(e) => patch({ targetHost: e.target.value })}
            />
          </label>
          <label className="w-24">
            <span className="mb-0.5 block text-xs text-neutral-400">{t('dialogs.targetPort')}</span>
            <input
              className={input}
              type="number"
              min={1}
              max={65535}
              value={draft.targetPort}
              onChange={(e) => patch({ targetPort: e.target.value })}
            />
          </label>
        </div>
      )}

      <fieldset className="mb-3">
        <legend className="mb-1 text-xs text-neutral-400">{t('dialogs.startMode')}</legend>
        {(Object.keys(START_MODE_LABEL) as TunnelStartMode[]).map((m) => (
          <label key={m} className="mb-0.5 flex items-center gap-2 text-xs text-neutral-300">
            <input
              type="radio"
              name="tunnel-start-mode"
              checked={draft.startMode === m}
              onChange={() => patch({ startMode: m })}
            />
            {START_MODE_LABEL[m]}
            {m === 'withSession' && (
              <span className="text-neutral-500">{t('dialogs.withSessionHint')}</span>
            )}
          </label>
        ))}
      </fieldset>

      {conflict && !conflict.available && (
        <div className="mb-3 rounded border border-yellow-800 bg-yellow-950/40 p-2 text-xs text-yellow-200">
          <p className="mb-1">
            {t('dialogs.portConflict', {
              port: draft.bindPort,
              holderNote: conflict.holder
                ? t('dialogs.portConflictHolder', { holder: conflict.holder })
                : '',
            })}
          </p>
          <p className="mb-2 text-yellow-300/80">{t('dialogs.portConflictHint')}</p>
          <div className="flex gap-2">
            {conflict.suggestedPort && (
              <button
                type="button"
                className="rounded bg-yellow-700 px-2 py-0.5 text-white hover:bg-yellow-600"
                onClick={() => {
                  patch({ bindPort: String(conflict.suggestedPort) });
                  // 使用建议端口 = 接受并继续保存（显式传新草稿，避开闭包旧值）
                  void save({ ...draft, bindPort: String(conflict.suggestedPort) });
                }}
              >
                {t('dialogs.useSuggestedPort', { port: conflict.suggestedPort })}
              </button>
            )}
            <button
              type="button"
              className="rounded border border-neutral-600 px-2 py-0.5 hover:bg-neutral-800"
              onClick={() => void save()}
            >
              {t('dialogs.recheck')}
            </button>
            <button
              type="button"
              className="rounded px-2 py-0.5 text-neutral-400 hover:bg-neutral-800"
              onClick={() => setConflict(null)}
            >
              {t('dialogs.cancel')}
            </button>
          </div>
        </div>
      )}

      {error && (
        <p aria-live="polite" className="mb-2 text-xs text-red-400">
          {error}
        </p>
      )}

      <div className="flex justify-end gap-2">
        <button
          type="button"
          className="rounded px-3 py-1 text-neutral-300 hover:bg-neutral-800"
          onClick={onClose}
        >
          {t('dialogs.cancel')}
        </button>
        <button
          type="button"
          className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500 disabled:opacity-50"
          disabled={busy}
          onClick={() => void save()}
        >
          {t('dialogs.save')}
        </button>
      </div>

      {pendingRestart && (
        <ConfirmDialog
          title={t('dialogs.restartTunnelTitle')}
          confirmLabel={t('dialogs.restartNow')}
          onConfirm={() => void restartNow()}
          onCancel={() => {
            notify(t('dialogs.restartLaterNote'), 'info');
            onClose();
          }}
        >
          {t('dialogs.restartTunnelBody', {
            name: pendingRestart.name || `${pendingRestart.bindHost}:${pendingRestart.bindPort}`,
          })}
        </ConfirmDialog>
      )}
    </Dialog>
  );
}
