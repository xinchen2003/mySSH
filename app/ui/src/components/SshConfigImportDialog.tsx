import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Dialog } from './Dialog';
import { useAppStore, type SshConfigPreviewEntry } from '../state/app-store';
import { useT } from '../i18n';

/**
 * ssh_config 批量导入预览弹窗：解析 ~/.ssh/config 后勾选导入。
 * 冲突（与现有会话重名）与被解析器跳过的条目默认不勾选；skipped 行不可勾选。
 * 由外层常驻挂载 + sshImportOpen 自门控（同 QuickConnectDialog 模式），重挂载即重置态。
 */
export function SshConfigImportDialog() {
  const open = useAppStore((s) => s.sshImportOpen);
  const toggle = useAppStore((s) => s.toggleSshImport);
  if (!open) return null;
  return <SshConfigImportForm onClose={toggle} />;
}

function SshConfigImportForm({ onClose }: { onClose: () => void }) {
  const t = useT();
  const importSshConfig = useAppStore((s) => s.importSshConfig);
  const [entries, setEntries] = useState<SshConfigPreviewEntry[] | null>(null);
  const [error, setError] = useState('');
  const [checked, setChecked] = useState<Set<number>>(new Set());
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    invoke<SshConfigPreviewEntry[]>('ssh_config_preview', { path: null })
      .then((list) => {
        if (!alive) return;
        setEntries(list);
        // 默认勾选：可导入且不冲突的条目
        setChecked(new Set(list.flatMap((e, i) => (e.skipped || e.conflict ? [] : [i]))));
      })
      .catch((e: unknown) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
  }, []);

  const selectable = (entries ?? []).flatMap((e, i) => (e.skipped ? [] : [i]));

  const submit = async () => {
    if (!entries || busy || checked.size === 0) return;
    setBusy(true);
    // conflict 字段后端按未知键忽略；预览条目原样回传
    const ok = await importSshConfig(entries.filter((_, i) => checked.has(i)));
    setBusy(false);
    if (ok) onClose();
  };

  return (
    <Dialog title={t('dialogs.sshImportTitle')} onClose={onClose} panelClass="w-[44rem]">
      {error ? (
        <p role="alert" className="text-xs text-red-400">
          {t('dialogs.sshImportPreviewFailed', { error })}
        </p>
      ) : entries === null ? (
        <p className="text-xs text-neutral-500">{t('dialogs.sshImportLoading')}</p>
      ) : entries.length === 0 ? (
        <p className="text-xs text-neutral-500">{t('dialogs.sshImportEmpty')}</p>
      ) : (
        <>
          <div className="max-h-80 overflow-y-auto rounded border border-neutral-800">
            <table className="w-full text-left text-xs">
              <thead className="sticky top-0 bg-neutral-900 text-neutral-500">
                <tr>
                  <th className="w-8 px-2 py-1.5" aria-label={t('dialogs.sshImportSelectAll')} />
                  <th className="px-2 py-1.5">{t('dialogs.sshImportColAlias')}</th>
                  <th className="px-2 py-1.5">{t('dialogs.sshImportColAddress')}</th>
                  <th className="px-2 py-1.5">{t('dialogs.username')}</th>
                  <th className="px-2 py-1.5">{t('dialogs.sshImportColAuth')}</th>
                  <th className="px-2 py-1.5">{t('dialogs.sshImportColJump')}</th>
                  <th className="px-2 py-1.5">{t('dialogs.sshImportColStatus')}</th>
                </tr>
              </thead>
              <tbody>
                {entries.map((e, i) => (
                  <tr
                    key={`${e.alias}-${i}`}
                    className="border-t border-neutral-800 text-neutral-300"
                  >
                    <td className="px-2 py-1.5">
                      <input
                        type="checkbox"
                        className="accent-blue-500"
                        disabled={e.skipped !== null}
                        checked={checked.has(i)}
                        aria-label={e.alias}
                        onChange={() =>
                          setChecked((prev) => {
                            const next = new Set(prev);
                            if (next.has(i)) next.delete(i);
                            else next.add(i);
                            return next;
                          })
                        }
                      />
                    </td>
                    <td className="max-w-32 truncate px-2 py-1.5" title={e.alias}>
                      {e.alias}
                    </td>
                    <td className="max-w-40 truncate px-2 py-1.5" title={`${e.hostname}:${e.port}`}>
                      {e.hostname}:{e.port}
                    </td>
                    <td className="max-w-24 truncate px-2 py-1.5">{e.user}</td>
                    <td
                      className="max-w-28 truncate px-2 py-1.5"
                      title={e.identityFile ?? undefined}
                    >
                      {/* 认证列：私钥取文件名，无则密码（密码导入后由用户在档案里补录） */}
                      {e.identityFile
                        ? (e.identityFile.split(/[\\/]/).pop() ?? e.identityFile)
                        : t('dialogs.password')}
                    </td>
                    <td className="max-w-24 truncate px-2 py-1.5">{e.proxyJump ?? ''}</td>
                    <td className="px-2 py-1.5">
                      {e.skipped ? (
                        <span
                          className="rounded bg-neutral-700 px-1.5 py-0.5 text-neutral-400"
                          title={e.skipped}
                        >
                          {t('dialogs.sshImportSkipped')}: {e.skipped}
                        </span>
                      ) : e.conflict ? (
                        <span
                          className="rounded bg-yellow-900/60 px-1.5 py-0.5 text-yellow-300"
                          title={t('dialogs.sshImportConflictHint')}
                        >
                          {t('dialogs.sshImportConflict')}
                        </span>
                      ) : null}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="mt-3 flex items-center justify-between text-xs">
            <div className="flex gap-2">
              <button
                className="rounded px-2 py-1 text-neutral-400 hover:bg-neutral-800"
                onClick={() => setChecked(new Set(selectable))}
              >
                {t('dialogs.sshImportSelectAll')}
              </button>
              <button
                className="rounded px-2 py-1 text-neutral-400 hover:bg-neutral-800"
                onClick={() => setChecked(new Set())}
              >
                {t('dialogs.sshImportSelectNone')}
              </button>
            </div>
            <div className="flex gap-2">
              <button
                className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
                onClick={onClose}
              >
                {t('dialogs.cancel')}
              </button>
              <button
                data-autofocus
                className="rounded bg-blue-700 px-3 py-1 text-white hover:bg-blue-600 disabled:opacity-50"
                disabled={checked.size === 0 || busy}
                onClick={() => void submit()}
              >
                {t('dialogs.sshImportSubmit', { count: checked.size })}
              </button>
            </div>
          </div>
        </>
      )}
    </Dialog>
  );
}
