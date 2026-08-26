import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { Dialog } from './Dialog';
import { ConfirmDialog } from './ConfirmDialog';
import { START_MODE_LABEL, startModeOf, tunnelDisplayName } from '../state/tunnel-utils';
import type { AuthSpec, SessionRecord, TunnelDef } from '../term/types';

type AuthKind = AuthSpec['type'];
type EditorTab = 'basic' | 'auth' | 'jump' | 'tunnels';

const TAB_LABEL: Record<EditorTab, string> = {
  basic: '基本信息',
  auth: '认证',
  jump: '跳板链',
  tunnels: '隧道',
};

/** 新建/编辑会话对话框。key=id 重挂载重置表单（editing 预填）。 */
export function ConnectDialog() {
  const show = useAppStore((s) => s.showConnect);
  const editing = useAppStore((s) => s.editing);
  const presetGroup = useAppStore((s) => s.connectPreset);
  if (!show) return null;
  // key 含 preset：分组菜单「新建连接」重挂载时重置表单并预填分组
  return (
    <ConnectForm
      key={editing?.id ?? `new-${presetGroup ?? ''}`}
      initial={editing}
      presetGroup={presetGroup}
    />
  );
}

function ConnectForm({
  initial,
  presetGroup,
}: {
  initial: SessionRecord | null;
  presetGroup: string | null;
}) {
  const close = useAppStore((s) => s.closeConnect);
  const connect = useAppStore((s) => s.connect);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const loadSessions = useAppStore((s) => s.loadSessions);

  const [tab, setTab] = useState<EditorTab>('basic');
  const [name, setName] = useState(initial?.name ?? '');
  const [groupPath, setGroupPath] = useState(initial?.groupPath ?? presetGroup ?? '');
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
  const [jumpChain, setJumpChain] = useState<string[]>(initial?.jumpChain ?? []);
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
        jumpChain,
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

  const input = 'w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1';

  return (
    <Dialog
      title={initial ? '编辑会话' : '新建 SSH 连接'}
      onClose={close}
      closeOnBackdrop={false}
      backdropClass="z-10"
      panelClass="w-[26rem] rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
    >
      <h2 className="mb-2 text-base font-semibold">{initial ? '编辑会话' : '新建 SSH 连接'}</h2>

      {/* §9.1 双入口之一：服务器编辑器标签页 */}
      <div className="mb-3 flex gap-1 border-b border-neutral-800" role="tablist">
        {(Object.keys(TAB_LABEL) as EditorTab[]).map((t) => (
          <button
            key={t}
            role="tab"
            aria-selected={tab === t}
            className={`px-2.5 py-1 text-xs ${
              tab === t
                ? 'border-b-2 border-blue-500 text-neutral-100'
                : 'text-neutral-500 hover:text-neutral-300'
            }`}
            onClick={() => setTab(t)}
          >
            {TAB_LABEL[t]}
          </button>
        ))}
      </div>

      <div className="flex min-h-56 flex-col gap-2">
        {tab === 'basic' && (
          <>
            {save && (
              <div className="flex gap-2">
                <label className="flex-1">
                  <span className="mb-0.5 block text-xs text-neutral-400">名称</span>
                  <input
                    className={input}
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    placeholder="缺省取 用户@主机"
                  />
                </label>
                <label className="w-28">
                  <span className="mb-0.5 block text-xs text-neutral-400">分组</span>
                  <input
                    className={input}
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
                  className={input}
                  value={host}
                  onChange={(e) => setHost(e.target.value)}
                  autoFocus
                />
              </label>
              <label className="w-20">
                <span className="mb-0.5 block text-xs text-neutral-400">端口</span>
                <input
                  className={input}
                  type="number"
                  value={port}
                  onChange={(e) => setPort(Number(e.target.value) || 22)}
                />
              </label>
            </div>
            <label>
              <span className="mb-0.5 block text-xs text-neutral-400">用户名</span>
              <input className={input} value={user} onChange={(e) => setUser(e.target.value)} />
            </label>
            <label className="mt-1 flex items-center gap-2 text-xs text-neutral-400">
              <input
                type="checkbox"
                checked={save}
                onChange={(e) => setSave(e.target.checked)}
                disabled={initial !== null}
              />
              保存会话（密码/passphrase 存加密保险库）
            </label>
          </>
        )}

        {tab === 'auth' && (
          <>
            <label>
              <span className="mb-0.5 block text-xs text-neutral-400">认证方式</span>
              <select
                className={input}
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
                  className={input}
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
                    className={input}
                    value={keyPath}
                    onChange={(e) => setKeyPath(e.target.value)}
                    placeholder="C:\Users\…\.ssh\id_ed25519 或 .ppk"
                  />
                </label>
                <label>
                  <span className="mb-0.5 block text-xs text-neutral-400">passphrase（可空）</span>
                  <input
                    className={input}
                    type="password"
                    value={passphrase}
                    onChange={(e) => setPassphrase(e.target.value)}
                  />
                </label>
              </>
            )}
          </>
        )}

        {tab === 'jump' &&
          (save ? (
            <JumpChainEditor
              chain={jumpChain}
              onChange={setJumpChain}
              excludeId={initial?.id ?? null}
            />
          ) : (
            <p className="py-6 text-center text-xs text-neutral-500">
              勾选「保存会话」后可配置跳板链
            </p>
          ))}

        {tab === 'tunnels' &&
          (initial ? (
            <SessionTunnelsTab sessionId={initial.id} />
          ) : (
            <p className="py-6 text-center text-xs text-neutral-500">
              保存会话后可在此配置隧道；隧道绑定到该服务器
            </p>
          ))}

        {error && <p className="text-xs text-red-400">{error}</p>}
      </div>

      <div className="mt-2 flex justify-end gap-2">
        <button className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800" onClick={close}>
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
    </Dialog>
  );
}

/** 隧道页（§9.1）：只列绑定当前服务器的规则；启动/停止/编辑/复制/删除 + 新建。 */
function SessionTunnelsTab({ sessionId }: { sessionId: string }) {
  const tunnelDefs = useAppStore((s) => s.tunnelDefs);
  const tunnels = useAppStore((s) => s.tunnels);
  const stopTunnel = useAppStore((s) => s.stopTunnel);
  const saveTunnel = useAppStore((s) => s.saveTunnel);
  const deleteTunnel = useAppStore((s) => s.deleteTunnel);
  const duplicateTunnel = useAppStore((s) => s.duplicateTunnel);
  const loadTunnelDefs = useAppStore((s) => s.loadTunnelDefs);
  const notify = useAppStore((s) => s.notify);

  const [editor, setEditor] = useState<{ def: TunnelDef | null } | undefined>(undefined);
  const [pendingDelete, setPendingDelete] = useState<TunnelDef | null>(null);

  // 面板外打开时定义可能未加载
  useEffect(() => {
    void loadTunnelDefs();
  }, [loadTunnelDefs]);

  const defs = tunnelDefs.filter((d) => d.sessionId === sessionId);
  const runtimeById = new Map(tunnels.map((t) => [t.tunnelId, t]));

  const startDef = async (d: TunnelDef) => {
    try {
      await saveTunnel(d, true);
    } catch (e) {
      notify(`启动失败: ${String(e)}`, 'error');
    }
  };

  return (
    <div className="text-xs">
      <div className="mb-1 flex items-center justify-between">
        <span className="text-neutral-500">仅显示绑定本服务器的规则</span>
        <button
          className="rounded border border-neutral-700 px-2 py-0.5 hover:bg-neutral-800"
          onClick={() => setEditor({ def: null })}
        >
          ＋ 新建隧道
        </button>
      </div>
      {defs.length === 0 ? (
        <p className="py-4 text-center text-neutral-600">暂无隧道</p>
      ) : (
        <ul>
          {defs.map((d) => {
            const rt = runtimeById.get(d.id);
            return (
              <li
                key={d.id}
                className="mb-1 flex items-center gap-2 rounded border border-neutral-800 px-2 py-1"
              >
                <span className="flex-1 truncate" title={rt?.lastError ?? undefined}>
                  <span className="text-neutral-200">{tunnelDisplayName(d)}</span>
                  <span className="ml-2 font-mono text-neutral-500">
                    {d.bindHost}:{d.bindPort}
                    {d.targetHost ? ` → ${d.targetHost}:${d.targetPort}` : ''}
                  </span>
                </span>
                <span className="text-neutral-500">{START_MODE_LABEL[startModeOf(d)]}</span>
                <span
                  className={
                    rt
                      ? rt.status === 'listening'
                        ? 'text-green-400'
                        : rt.status === 'failed'
                          ? 'text-red-400'
                          : 'text-yellow-400'
                      : 'text-neutral-600'
                  }
                >
                  {rt ? rt.status : '未运行'}
                </span>
                {rt ? (
                  <button
                    className="rounded px-1 text-neutral-500 hover:text-red-400"
                    onClick={() => void stopTunnel(d.id)}
                  >
                    停止
                  </button>
                ) : (
                  <button
                    className="rounded px-1 text-neutral-500 hover:text-green-400"
                    onClick={() => void startDef(d)}
                  >
                    启动
                  </button>
                )}
                <button
                  className="rounded px-1 text-neutral-500 hover:text-neutral-200"
                  onClick={() => setEditor({ def: d })}
                >
                  编辑
                </button>
                <button
                  className="rounded px-1 text-neutral-500 hover:text-neutral-200"
                  onClick={() => void duplicateTunnel(d)}
                >
                  复制
                </button>
                <button
                  className="rounded px-1 text-neutral-500 hover:text-red-400"
                  onClick={() => setPendingDelete(d)}
                >
                  删除
                </button>
              </li>
            );
          })}
        </ul>
      )}

      {editor && (
        <TunnelEditor
          sessionId={sessionId}
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

/** 跳板链编辑：从已存会话中按序挑选（就近→最远）；排除自身与已选 */
function JumpChainEditor({
  chain,
  onChange,
  excludeId,
}: {
  chain: string[];
  onChange: (chain: string[]) => void;
  excludeId: string | null;
}) {
  const sessions = useAppStore((s) => s.sessions);
  const candidates = sessions.filter((s) => s.id !== excludeId && !chain.includes(s.id));
  const nameOf = (id: string) => sessions.find((s) => s.id === id)?.name ?? id;

  return (
    <div className="mt-1 rounded border border-neutral-800 p-2">
      <div className="mb-1 text-xs text-neutral-400">跳板链（ProxyJump，就近→最远）</div>
      {chain.map((id, i) => (
        <div key={id} className="mb-1 flex items-center gap-2 text-xs">
          <span className="text-neutral-500">{i + 1}.</span>
          <span className="flex-1 truncate">{nameOf(id)}</span>
          <button
            className="text-neutral-500 hover:text-red-400"
            onClick={() => onChange(chain.filter((x) => x !== id))}
          >
            移除
          </button>
        </div>
      ))}
      {candidates.length > 0 && (
        <select
          className="w-full rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1 text-xs"
          value=""
          onChange={(e) => e.target.value && onChange([...chain, e.target.value])}
        >
          <option value="">+ 添加跳板…</option>
          {candidates.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}（{s.username}@{s.host}:{s.port}）
            </option>
          ))}
        </select>
      )}
    </div>
  );
}
