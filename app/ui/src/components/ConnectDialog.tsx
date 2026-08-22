import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import type { AuthSpec, SessionRecord } from '../term/types';

type AuthKind = AuthSpec['type'];

/** 新建/编辑会话对话框。key=id 重挂载重置表单（editing 预填）。 */
export function ConnectDialog() {
  const show = useAppStore((s) => s.showConnect);
  const editing = useAppStore((s) => s.editing);
  if (!show) return null;
  return <ConnectForm key={editing?.id ?? 'new'} initial={editing} />;
}

function ConnectForm({ initial }: { initial: SessionRecord | null }) {
  const close = useAppStore((s) => s.closeConnect);
  const connect = useAppStore((s) => s.connect);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const loadSessions = useAppStore((s) => s.loadSessions);

  const [name, setName] = useState(initial?.name ?? '');
  const [groupPath, setGroupPath] = useState(initial?.groupPath ?? '');
  const [host, setHost] = useState(initial?.host ?? '127.0.0.1');
  const [port, setPort] = useState(initial?.port ?? 22);
  const [user, setUser] = useState(initial?.username ?? '');
  const [kind, setKind] = useState<AuthKind>(
    initial?.authType === 'publickey'
      ? 'publicKey'
      : initial?.authType === 'keyboard-interactive'
        ? 'keyboardInteractive'
        : (initial?.authType ?? 'password'),
  );
  const [password, setPassword] = useState('');
  const [keyPath, setKeyPath] = useState(initial?.keyPath ?? '');
  const [passphrase, setPassphrase] = useState('');
  const [save, setSave] = useState(initial !== null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    setError(null);
    setBusy(true);
    try {
      let auth: AuthSpec;
      if (kind === 'password') auth = { type: 'password', password };
      else if (kind === 'agent') auth = { type: 'agent' };
      else if (kind === 'keyboardInteractive') auth = { type: 'keyboardInteractive' };
      else {
        if (!keyPath.trim()) throw new Error('请填写私钥文件路径');
        const keyPem = await invoke<string>('read_private_key', { path: keyPath.trim() });
        auth = { type: 'publicKey', keyPem, passphrase: passphrase || null };
      }

      if (!save) {
        connect({ host: host.trim(), port, user: user.trim(), auth });
        return;
      }

      // 保存档案：秘密经 cred_set 进保险库（不经 sessions 表）
      const id = initial?.id ?? `s-${Date.now()}`;
      const record: SessionRecord = {
        id,
        name: name.trim() || `${user.trim()}@${host.trim()}`,
        host: host.trim(),
        port,
        username: user.trim(),
        authType:
          kind === 'publicKey'
            ? 'publickey'
            : kind === 'keyboardInteractive'
              ? 'keyboard-interactive'
              : kind,
        keyPath: kind === 'publicKey' ? keyPath.trim() : null,
        groupPath: groupPath.trim(),
        tags: initial?.tags ?? [],
        command: initial?.command ?? null,
        createdAt: initial?.createdAt ?? '',
        updatedAt: '',
      };
      await invoke('session_upsert', { record });
      if (kind === 'password' && password)
        await invoke('cred_set', { sessionId: id, kind: 'password', secret: password });
      if (kind === 'publicKey' && passphrase)
        await invoke('cred_set', { sessionId: id, kind: 'keyPassphrase', secret: passphrase });
      await loadSessions();
      connectBySession(id, record.name);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="fixed inset-0 z-10 flex items-center justify-center bg-black/60">
      <div className="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl">
        <h2 className="mb-3 text-base font-semibold">{initial ? '编辑会话' : '新建 SSH 连接'}</h2>
        <div className="flex flex-col gap-2">
          {save && (
            <div className="flex gap-2">
              <label className="flex-1">
                <span className="mb-0.5 block text-xs text-neutral-400">名称</span>
                <input
                  className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder="缺省取 用户@主机"
                />
              </label>
              <label className="w-28">
                <span className="mb-0.5 block text-xs text-neutral-400">分组</span>
                <input
                  className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  value={groupPath}
                  onChange={(e) => setGroupPath(e.target.value)}
                  placeholder="生产/华东"
                />
              </label>
            </div>
          )}
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
              <span className="mb-0.5 block text-xs text-neutral-400">
                密码{initial ? '（留空 = 沿用已存凭据）' : ''}
              </span>
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
          <label className="mt-1 flex items-center gap-2 text-xs text-neutral-400">
            <input
              type="checkbox"
              checked={save}
              onChange={(e) => setSave(e.target.checked)}
              disabled={initial !== null}
            />
            保存会话（密码/passphrase 存加密保险库）
          </label>
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
              {save ? '保存并连接' : '连接'}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
