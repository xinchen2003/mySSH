import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import { Dialog } from './Dialog';
import { useT } from '../i18n';

/**
 * 主机密钥确认弹窗（安全模型第 3 条）：
 * 首连展示指纹，变更时硬警告（红）并展示新旧指纹对比。
 *
 * 批次四 10.5 特殊规则：
 * - 默认焦点在「拒绝」（data-autofocus），打开瞬间误按 Enter = 拒绝；
 * - Enter 不得直接永久信任（信任按钮无默认焦点、无 enterAction）；
 * - Esc 等价于拒绝；
 * - 密钥变更硬警告、120 秒 fail-closed（后端）保持不变。
 */
export function HostKeyDialog() {
  const t = useT();
  const pending = useAppStore((s) => s.pendingHostKeys[0] ?? null);
  const clear = useAppStore((s) => s.shiftHostKey);

  if (!pending) return null;
  const changed = pending.kind === 'changed';

  const answer = (accept: boolean, remember: boolean) => {
    void invoke('hostkey_confirm', {
      confirmId: pending.confirmId,
      accept,
      remember,
    });
    clear();
  };
  const reject = () => answer(false, false);

  return (
    <Dialog
      title={t('dialogs.hostKeyTitle')}
      onClose={reject}
      closeOnBackdrop={false}
      backdropClass="z-20"
      panelClass={`w-[28rem] rounded-lg border p-4 text-sm shadow-xl ${
        changed
          ? 'border-red-700 bg-red-950 text-red-100'
          : 'border-neutral-700 bg-neutral-900 text-neutral-200'
      }`}
    >
      {changed ? (
        <>
          <h2 className="mb-2 text-base font-semibold text-red-300">
            {t('dialogs.hostKeyChanged')}
          </h2>
          <p className="mb-2 text-xs">
            {pending.host}:{pending.port}（{pending.keyType}）
          </p>
          <div className="mb-3 flex flex-col gap-1 rounded bg-black/40 p-2 font-mono text-xs">
            <span>{t('dialogs.oldFingerprint', { fp: pending.oldFingerprint ?? '' })}</span>
            <span className="text-red-300">
              {t('dialogs.newFingerprint', { fp: pending.newFingerprint ?? '' })}
            </span>
          </div>
          <div className="flex justify-end gap-2">
            <button
              data-autofocus
              className="rounded bg-neutral-700 px-3 py-1 hover:bg-neutral-600"
              onClick={reject}
            >
              {t('dialogs.rejectConnection')}
            </button>
            <button
              className="rounded bg-red-600 px-3 py-1 text-white hover:bg-red-500"
              onClick={() => answer(true, true)}
            >
              {t('dialogs.verifiedUpdateConnect')}
            </button>
          </div>
        </>
      ) : (
        <>
          <h2 className="mb-2 text-base font-semibold">{t('dialogs.firstConnectTitle')}</h2>
          <p className="mb-2 text-xs text-neutral-400">
            {pending.host}:{pending.port}（{pending.keyType}）
          </p>
          <div className="mb-3 rounded bg-black/40 p-2 font-mono text-xs">
            {pending.fingerprint}
          </div>
          <div className="flex justify-end gap-2">
            <button
              data-autofocus
              className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
              onClick={reject}
            >
              {t('dialogs.reject')}
            </button>
            <button
              className="rounded border border-neutral-600 px-3 py-1 hover:bg-neutral-800"
              onClick={() => answer(true, false)}
            >
              {t('dialogs.trustOnce')}
            </button>
            <button
              className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500"
              onClick={() => answer(true, true)}
            >
              {t('dialogs.trustAndRemember')}
            </button>
          </div>
        </>
      )}
    </Dialog>
  );
}
