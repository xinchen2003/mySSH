import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import type { AuthSpec } from '../term/types';

type AuthKind = AuthSpec['type'];

/**
 * 新建连接对话框。M1：密码 / 公钥（文件路径）/ keyboard-interactive / agent。
 * 会话持久化（session store + 加密凭据）在 M2 落地，届时此表单改为从会话档案启动。
 */
export function ConnectDialog() {
  const show = useAppStore((s) => s.showConnect);
  const close = useAppStore((s) => s.closeConnect);
  const connect = useAppStore((s) => s.connect);

  const [host, setHost] = useState('127.0.0.1');
  const [port, setPort] = useState(22);
  const [user, setUser] = useState('');
  const [kind, setKind] = useState<AuthKind>('password');
  const [password, setPassword] = useState('');
  const [keyPath, setKeyPath] = useState('');
  const [passphrase, setPassphrase] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (!show) return null;

  const submit = async () => {
    setError(null);
    let auth: AuthSpec;
    if (kind === 'password') auth = { type: 'password', password };
    else if (kind === 'agent') auth = { type: 'agent' };
    else if (kind === 'keyboardInteractive') auth = { type: 'keyboardInteractive' };
    else {
      if (!keyPath.trim()) {
        setError('请填写私钥文件路径');
        return;
      }
      setBusy(true);
      try {
        const keyPem = await invoke<string>('read_private_key', { path: keyPath.trim() });
        auth = {
          type: 'publicKey',
          keyPem,
          passphrase: passphrase || null,
        };
      } catch (e) {
        setBusy(false);
        setError(String(e));
        return;
      }
    }
    setBusy(false);
    connect({ host: host.trim(), port, user: user.trim(), auth });
  };

  return (
    <div className="fixed inset-0 z-10 flex items-center justify-center bg-black/60">
      <div className="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl">
        <h2 className="mb-3 text-base font-semibold">新建 SSH 连接</h2>
        <div className="flex flex-col gap-2">
          <div className="flex gap-2">
            <label className="flex-1">
              <span className="mb-0.5 block text-xs text-neutral-400">主机</span>
              <input
                className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                value={host}
                onChange={(e) => setHost(e.target.value)}
                autoFocus
              />
            </label>
            <label className="w-20">
              <span className="mb-0.5 block text-xs text-neutral-400">端口</span>
              <input
                className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                type="number"
                value={port}
                onChange={(e) => setPort(Number(e.target.value) || 22)}
              />
            </label>
          </div>
          <label>
            <span className="mb-0.5 block text-xs text-neutral-400">用户名</span>
            <input
              className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
              value={user}
              onChange={(e) => setUser(e.target.value)}
            />
          </label>
          <label>
            <span className="mb-0.5 block text-xs text-neutral-400">认证方式</span>
            <select
              className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
              value={kind}
              onChange={(e) => setKind(e.target.value as AuthKind)}
            >
              <option value="password">密码</option>
              <option value="publicKey">公钥（OpenSSH / .ppk）</option>
              <option value="keyboardInteractive">keyboard-interactive（2FA）</option>
              <option value="agent">ssh-agent / Pageant</option>
            </select>
          </label>
          {kind === 'password' && (
            <label>
              <span className="mb-0.5 block text-xs text-neutral-400">密码</span>
              <input
                className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
          )}
          {kind === 'publicKey' && (
            <>
              <label>
                <span className="mb-0.5 block text-xs text-neutral-400">私钥文件路径</span>
                <input
                  className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  value={keyPath}
                  onChange={(e) => setKeyPath(e.target.value)}
                  placeholder="C:\Users\…\.ssh\id_ed25519 或 .ppk"
                />
              </label>
              <label>
                <span className="mb-0.5 block text-xs text-neutral-400">passphrase（可空）</span>
                <input
                  className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  type="password"
                  value={passphrase}
                  onChange={(e) => setPassphrase(e.target.value)}
                />
              </label>
            </>
          )}
          {error && <p className="text-xs text-red-400">{error}</p>}
          <div className="mt-2 flex justify-end gap-2">
            <button
              className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
              onClick={close}
            >
              取消
            </button>
            <button
              className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500 disabled:opacity-50"
              onClick={() => void submit()}
              disabled={busy || !host.trim() || !user.trim()}
            >
              连接
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
