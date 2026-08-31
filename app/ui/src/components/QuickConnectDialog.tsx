import { useState } from 'react';
import { Dialog } from './Dialog';
import { useAppStore } from '../state/app-store';
import { GROUP_KEYS, readStringList } from '../state/groups';
import { useT } from '../i18n';

const inputCls =
  'w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1 text-xs text-neutral-200 outline-none focus:border-neutral-500 focus-visible:ring-1 focus-visible:ring-neutral-500';

/**
 * 快速连接（12.5 空态）：不保存档案的临时连接。
 * 密码留空走 keyboard-interactive（服务端届时经 KiDialog 逐题询问）。
 */
export function QuickConnectDialog() {
  const open = useAppStore((s) => s.quickConnectOpen);
  const toggle = useAppStore((s) => s.toggleQuickConnect);
  if (!open) return null;
  return <QuickConnectForm onClose={toggle} />;
}

function QuickConnectForm({ onClose }: { onClose: () => void }) {
  const t = useT();
  const [host, setHost] = useState('');
  const [port, setPort] = useState('22');
  const [user, setUser] = useState('');
  const [password, setPassword] = useState('');
  const [error, setError] = useState('');
  // 批次十一 7：最近连接历史（sessions.recent 持久化于 settings KV），选中回填 host/port/user；
  // 档案已删除的条目直接跳过；定位不变——回填后仍是不保存档案的临时连接
  const sessions = useAppStore((s) => s.sessions);
  const recentIds = useAppStore((s) => s.settings[GROUP_KEYS.recent]);
  const recent = readStringList(recentIds)
    .map((id) => sessions.find((r) => r.id === id))
    .filter((r): r is (typeof sessions)[number] => r !== undefined);

  const submit = () => {
    const p = Number(port);
    if (!host.trim() || !user.trim()) {
      setError(t('dialogs.hostUserRequired'));
      return;
    }
    if (!Number.isInteger(p) || p < 1 || p > 65535) {
      setError(t('dialogs.invalidPort'));
      return;
    }
    useAppStore.getState().connect({
      host: host.trim(),
      port: p,
      user: user.trim(),
      auth: password ? { type: 'password', password } : { type: 'keyboardInteractive' },
    });
    onClose();
  };

  return (
    <Dialog
      title={t('dialogs.quickConnectTitle')}
      onClose={onClose}
      enterAction={submit}
      panelClass="w-96"
    >
      <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2 text-xs text-neutral-400">
        {recent.length > 0 && (
          <>
            <label htmlFor="qc-recent">{t('dialogs.recentConnections')}</label>
            <select
              id="qc-recent"
              className={inputCls}
              value=""
              onChange={(e) => {
                const rec = recent.find((r) => r.id === e.target.value);
                if (!rec) return;
                setHost(rec.host);
                setPort(String(rec.port));
                setUser(rec.username);
              }}
            >
              <option value="">{t('dialogs.recentFill')}</option>
              {recent.map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}（{r.username}@{r.host}:{r.port}）
                </option>
              ))}
            </select>
          </>
        )}
        <label htmlFor="qc-host">{t('dialogs.host')}</label>
        <input
          id="qc-host"
          data-autofocus
          className={inputCls}
          autoComplete="off"
          spellCheck={false}
          placeholder={t('dialogs.hostPlaceholder')}
          value={host}
          onChange={(e) => setHost(e.target.value)}
        />
        <label htmlFor="qc-port">{t('dialogs.port')}</label>
        <input
          id="qc-port"
          className={inputCls}
          inputMode="numeric"
          value={port}
          onChange={(e) => setPort(e.target.value)}
        />
        <label htmlFor="qc-user">{t('dialogs.username')}</label>
        <input
          id="qc-user"
          className={inputCls}
          autoComplete="username"
          spellCheck={false}
          value={user}
          onChange={(e) => setUser(e.target.value)}
        />
        <label htmlFor="qc-pass">{t('dialogs.password')}</label>
        <input
          id="qc-pass"
          type="password"
          className={inputCls}
          autoComplete="current-password"
          placeholder={t('dialogs.passwordAskPlaceholder')}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
      </div>
      {error && (
        <p aria-live="polite" className="mt-2 text-xs text-red-400">
          {error}
        </p>
      )}
      <p className="mt-2 text-xs text-neutral-600">{t('dialogs.quickConnectNote')}</p>
      <div className="mt-3 flex justify-end gap-2 text-xs">
        <button
          className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
          onClick={onClose}
        >
          {t('dialogs.cancel')}
        </button>
        <button
          className="rounded bg-blue-700 px-3 py-1 text-white hover:bg-blue-600"
          onClick={submit}
        >
          {t('dialogs.connect')}
        </button>
      </div>
    </Dialog>
  );
}
