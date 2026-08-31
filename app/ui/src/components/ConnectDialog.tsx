import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import { TunnelEditor } from './TunnelEditor';
import { Dialog } from './Dialog';
import { ConfirmDialog } from './ConfirmDialog';
import { START_MODE_LABEL, startModeOf, tunnelDisplayName } from '../state/tunnel-utils';
import { useT, type MsgKey } from '../i18n';
import type { AuthSpec, SessionRecord, TunnelDef } from '../term/types';

type AuthKind = AuthSpec['type'];
type EditorTab = 'basic' | 'auth' | 'jump' | 'tunnels';
/** 终端编码选项（encoding_rs 标签；utf-8 = 直通不转码） */
const ENCODING_OPTIONS = [
  ['utf-8', 'dialogs.encodingUtf8'],
  ['gbk', 'dialogs.encodingGbk'],
  ['gb18030', 'dialogs.encodingGb18030'],
  ['big5', 'dialogs.encodingBig5'],
  ['shift_jis', 'dialogs.encodingShiftJis'],
  ['euc-kr', 'dialogs.encodingEucKr'],
] as const;

const TAB_LABEL: Record<EditorTab, MsgKey> = {
  basic: 'dialogs.tabBasic',
  auth: 'dialogs.tabAuth',
  jump: 'dialogs.tabJump',
  tunnels: 'dialogs.tabTunnels',
};
/** session_test_connect 请求（Rust TestConnectRequest，serde camelCase） */
interface TestConnectRequest {
  sessionId?: string;
  host: string;
  port: number;
  user: string;
  /** 与 SessionRecord.authType 相同的 serde 形式（'publickey' / 'keyboard-interactive'） */
  authType: SessionRecord['authType'];
  password?: string;
  keyPath?: string;
  passphrase?: string;
  jumpChain?: string[];
}

/** session_test_connect 返回（Ok 值，不 Err） */
interface TestConnectResult {
  ok: boolean;
  latencyMs?: number;
  error?: string;
}

type TestState =
  | { phase: 'idle' }
  | { phase: 'testing' }
  | { phase: 'ok'; latencyMs: number | null }
  | { phase: 'err'; message: string };

/** 标签颜色预设（选「无」= null） */
const PRESET_COLORS = [
  '#e5484d',
  '#e93d82',
  '#ab4aba',
  '#6e56cf',
  '#3e63dd',
  '#0090ff',
  '#12a594',
  '#30a46c',
];

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
  const t = useT();

  const [tab, setTab] = useState<EditorTab>('basic');
  const [name, setName] = useState(initial?.name ?? '');
  // 会话类型：创建后锁定（认证/凭据/隧道语义完全不同，互转会留下脏数据）
  const [sessKind, setSessKind] = useState<'ssh' | 'local'>(initial?.kind ?? 'ssh');
  // local 表单字段
  const [workdir, setWorkdir] = useState(initial?.workdir ?? '');
  const [shell, setShell] = useState(initial?.shell ?? '');
  const [command, setCommand] = useState(initial?.command ?? '');
  const [nameTouched, setNameTouched] = useState(false);
  const [color, setColor] = useState<string | null>(initial?.color ?? null);
  const [encoding, setEncoding] = useState(initial?.encoding ?? 'utf-8');
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
  // 本对话框是完整编辑器，默认落库（一次性连接走 QuickConnectDialog）；默认保存后名称/颜色设置可见
  const [save, setSave] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [test, setTest] = useState<TestState>({ phase: 'idle' });

  // 新建时名称跟随 用户@主机；用户手动改过后停止跟随（编辑已有会话不跟随）。
  // 在 host/user 的 onChange 里同步推导，避免 effect 里同步 setState（react-hooks v6）
  const followName = (u: string, h: string) => {
    if (initial || nameTouched) return;
    const ut = u.trim();
    const ht = h.trim();
    setName(ut && ht ? `${ut}@${ht}` : '');
  };

  /** 按表单当前值测试连接；不改表单状态，结果内联显示 */
  const testConnect = async () => {
    setTest({ phase: 'testing' });
    try {
      const req: TestConnectRequest = {
        host: host.trim(),
        port,
        user: user.trim(),
        authType:
          kind === 'publicKey'
            ? 'publickey'
            : kind === 'keyboardInteractive'
              ? 'keyboard-interactive'
              : kind,
      };
      // 编辑已有会话且密码留空时，后端按 sessionId 回退保险库
      if (initial) req.sessionId = initial.id;
      if (kind === 'password' && password) req.password = password;
      if (kind === 'publicKey') {
        if (keyPath.trim()) req.keyPath = keyPath.trim();
        if (passphrase) req.passphrase = passphrase;
      }
      if (jumpChain.length > 0) req.jumpChain = jumpChain;
      const r = await invoke<TestConnectResult>('session_test_connect', { req });
      setTest(
        r.ok
          ? { phase: 'ok', latencyMs: r.latencyMs ?? null }
          : { phase: 'err', message: r.error ?? t('dialogs.connectFailed') },
      );
    } catch (e) {
      setTest({ phase: 'err', message: String(e) });
    }
  };

  /** connectAfter=false 时仅落库不发起连接（服务器暂不可达也应能保存） */
  const submit = async (connectAfter: boolean) => {
    setError(null);
    setBusy(true);
    try {
      if (sessKind === 'local') {
        // 本地会话：无认证/凭据；host/port/username/authType 存占位值（DB 列 NOT NULL）
        const id = initial?.id ?? `s-${Date.now()}`;
        const record: SessionRecord = {
          id,
          name: name.trim() || t('dialogs.localTerminal'),
          kind: 'local',
          host: 'localhost',
          port: 0,
          username: '',
          authType: 'password',
          keyPath: null,
          shell: shell || null,
          workdir: workdir.trim() || null,
          jumpChain: [],
          groupPath: presetGroup ?? initial?.groupPath ?? '',
          color,
          encoding,
          tags: initial?.tags ?? [],
          command: command.trim() || null,
          createdAt: initial?.createdAt ?? '',
          updatedAt: '',
        };
        await invoke('session_upsert', { record });
        await loadSessions();
        connectBySession(id, record.name);
        return;
      }
      if (!save) {
        // 临时连接（不落库）：认证材料现取，公钥需读文件
        let auth: AuthSpec;
        if (kind === 'password') auth = { type: 'password', password };
        else if (kind === 'agent') auth = { type: 'agent' };
        else if (kind === 'keyboardInteractive') auth = { type: 'keyboardInteractive' };
        else {
          if (!keyPath.trim()) throw new Error(t('dialogs.keyPathRequired'));
          const keyPem = await invoke<string>('read_private_key', { path: keyPath.trim() });
          auth = { type: 'publicKey', keyPem, passphrase: passphrase || null };
        }
        connect({ host: host.trim(), port, user: user.trim(), auth, encoding });
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
        // 分组不再表单编辑：静默保留 preset（分组菜单新建）或原值
        groupPath: presetGroup ?? initial?.groupPath ?? '',
        color,
        encoding,
        tags: initial?.tags ?? [],
        command: initial?.command ?? null,
        createdAt: initial?.createdAt ?? '',
        updatedAt: '',
      };
      // 保存路径不读私钥、不要求可达：认证在真正连接时才解析
      await invoke('session_upsert', { record });
      if (kind === 'password' && password)
        await invoke('cred_set', { sessionId: id, kind: 'password', secret: password });
      if (kind === 'publicKey' && passphrase)
        await invoke('cred_set', { sessionId: id, kind: 'keyPassphrase', secret: passphrase });
      await loadSessions();
      if (connectAfter) connectBySession(id, record.name);
      else close();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const input = 'w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1';

  return (
    <Dialog
      title={
        initial
          ? t('dialogs.titleEdit')
          : sessKind === 'local'
            ? t('dialogs.titleNewLocal')
            : t('dialogs.titleNewSsh')
      }
      onClose={close}
      closeOnBackdrop={false}
      backdropClass="z-10"
      panelClass="w-[26rem] rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
    >
      <h2 className="mb-2 text-base font-semibold">
        {initial
          ? t('dialogs.titleEdit')
          : sessKind === 'local'
            ? t('dialogs.titleNewLocal')
            : t('dialogs.titleNewSsh')}
      </h2>
      {/* 会话类型（批次十四）：本地终端 = 本机 PTY，适合跑 AI agent 等命令行工具 */}
      {!initial && (
        <div className="mb-3 flex gap-1">
          {(
            [
              ['ssh', 'dialogs.kindSsh'],
              ['local', 'dialogs.localTerminal'],
            ] as const
          ).map(([k, label]) => (
            <button
              key={k}
              className={`rounded px-2.5 py-1 text-xs ${
                sessKind === k
                  ? 'bg-blue-600 text-white'
                  : 'border border-neutral-700 text-neutral-400 hover:text-neutral-200'
              }`}
              onClick={() => {
                setSessKind(k);
                setTab('basic');
              }}
            >
              {t(label)}
            </button>
          ))}
        </div>
      )}

      {/* §9.1 双入口之一：服务器编辑器标签页 */}
      <div className="mb-3 flex gap-1 border-b border-neutral-800" role="tablist">
        {(Object.keys(TAB_LABEL) as EditorTab[])
          .filter((tabKey) => sessKind === 'ssh' || tabKey === 'basic')
          .map((tabKey) => (
            <button
              key={tabKey}
              role="tab"
              aria-selected={tab === tabKey}
              className={`px-2.5 py-1 text-xs ${
                tab === tabKey
                  ? 'border-b-2 border-blue-500 text-neutral-100'
                  : 'text-neutral-500 hover:text-neutral-300'
              }`}
              onClick={() => setTab(tabKey)}
            >
              {t(TAB_LABEL[tabKey])}
            </button>
          ))}
      </div>

      <div className="flex min-h-56 flex-col gap-2">
        {tab === 'basic' && (
          <>
            {(save || sessKind === 'local') && (
              <label>
                <span className="mb-0.5 block text-xs text-neutral-400">
                  {t('dialogs.labelName')}
                </span>
                <input
                  className={input}
                  value={name}
                  onChange={(e) => {
                    setNameTouched(true);
                    setName(e.target.value);
                  }}
                  placeholder={t('dialogs.namePlaceholder')}
                />
              </label>
            )}
            {(save || sessKind === 'local') && (
              <div>
                <span className="mb-0.5 block text-xs text-neutral-400">
                  {t('dialogs.labelColor')}
                </span>
                <div className="flex items-center gap-1.5">
                  <button
                    type="button"
                    title={t('dialogs.colorNone')}
                    aria-label={t('dialogs.colorNoneAria')}
                    className={`flex h-5 w-5 items-center justify-center rounded-full border border-neutral-600 text-[10px] leading-none text-neutral-500 ${
                      color === null
                        ? 'ring-2 ring-neutral-200 ring-offset-1 ring-offset-neutral-900'
                        : ''
                    }`}
                    onClick={() => setColor(null)}
                  >
                    ∅
                  </button>
                  {PRESET_COLORS.map((c) => (
                    <button
                      key={c}
                      type="button"
                      title={c}
                      aria-label={t('dialogs.colorValueAria', { color: c })}
                      style={{ backgroundColor: c }}
                      className={`h-5 w-5 rounded-full ${
                        color === c
                          ? 'ring-2 ring-neutral-200 ring-offset-1 ring-offset-neutral-900'
                          : ''
                      }`}
                      onClick={() => setColor(c)}
                    />
                  ))}
                  {/* 调色板任意颜色 */}
                  <input
                    type="color"
                    title={t('dialogs.customColor')}
                    aria-label={t('dialogs.customColor')}
                    value={color ?? '#3e63dd'}
                    onChange={(e) => setColor(e.target.value)}
                    className={`h-5 w-7 cursor-pointer rounded border border-neutral-600 bg-transparent p-0 ${
                      color !== null && !PRESET_COLORS.includes(color)
                        ? 'ring-2 ring-neutral-200 ring-offset-1 ring-offset-neutral-900'
                        : ''
                    }`}
                  />
                </div>
              </div>
            )}
            {sessKind === 'local' && (
              <>
                <label>
                  <span className="mb-0.5 block text-xs text-neutral-400">
                    {t('dialogs.workdirLabel')}
                  </span>
                  <input
                    className={input}
                    value={workdir}
                    spellCheck={false}
                    onChange={(e) => setWorkdir(e.target.value)}
                    placeholder="D:\projects\my-app"
                  />
                </label>
                <label>
                  <span className="mb-0.5 block text-xs text-neutral-400">Shell</span>
                  <select
                    className={input}
                    value={shell}
                    onChange={(e) => setShell(e.target.value)}
                  >
                    <option value="">{t('dialogs.shellAuto')}</option>
                    <option value="pwsh">{t('dialogs.shellPwsh')}</option>
                    <option value="powershell">Windows PowerShell</option>
                    <option value="cmd">{t('dialogs.shellCmd')}</option>
                  </select>
                </label>
                <label>
                  <span className="mb-0.5 block text-xs text-neutral-400">
                    {t('dialogs.commandLabel')}
                  </span>
                  <input
                    className={input}
                    value={command}
                    spellCheck={false}
                    onChange={(e) => setCommand(e.target.value)}
                    placeholder={t('dialogs.commandPlaceholder')}
                  />
                </label>
                <p className="text-xs text-neutral-500">{t('dialogs.localNote')}</p>
              </>
            )}
            {sessKind === 'ssh' && (
              <>
                <div className="flex gap-2">
                  <label className="flex-1">
                    <span className="mb-0.5 block text-xs text-neutral-400">
                      {t('dialogs.host')}
                    </span>
                    <input
                      className={input}
                      value={host}
                      autoComplete="off"
                      spellCheck={false}
                      onChange={(e) => {
                        setHost(e.target.value);
                        followName(user, e.target.value);
                      }}
                      autoFocus
                    />
                  </label>
                  <label className="w-20">
                    <span className="mb-0.5 block text-xs text-neutral-400">
                      {t('dialogs.port')}
                    </span>
                    <input
                      className={input}
                      type="number"
                      value={port}
                      onChange={(e) => setPort(Number(e.target.value) || 22)}
                    />
                  </label>
                </div>
                <label>
                  <span className="mb-0.5 block text-xs text-neutral-400">
                    {t('dialogs.username')}
                  </span>
                  <input
                    className={input}
                    value={user}
                    autoComplete="username"
                    spellCheck={false}
                    onChange={(e) => {
                      setUser(e.target.value);
                      followName(e.target.value, host);
                    }}
                  />
                </label>
                <label className="mt-1 flex items-center gap-2 text-xs text-neutral-400">
                  <input
                    type="checkbox"
                    checked={save}
                    onChange={(e) => setSave(e.target.checked)}
                    disabled={initial !== null}
                  />
                  {t('dialogs.saveSessionLabel')}
                </label>
              </>
            )}
            {/* 终端编码：SSH 与本地会话共用；中文 Windows 本地终端通常选 GBK */}
            <label>
              <span className="mb-0.5 block text-xs text-neutral-400">
                {t('dialogs.encodingLabel')}
              </span>
              <select
                className={input}
                value={encoding}
                onChange={(e) => setEncoding(e.target.value)}
              >
                {ENCODING_OPTIONS.map(([value, label]) => (
                  <option key={value} value={value}>
                    {t(label)}
                  </option>
                ))}
              </select>
            </label>
          </>
        )}

        {tab === 'auth' && (
          <>
            <label>
              <span className="mb-0.5 block text-xs text-neutral-400">
                {t('dialogs.authMethod')}
              </span>
              <select
                className={input}
                value={kind}
                onChange={(e) => setKind(e.target.value as AuthKind)}
              >
                <option value="password">{t('dialogs.password')}</option>
                <option value="publicKey">{t('dialogs.authPublicKey')}</option>
                <option value="keyboardInteractive">{t('dialogs.authKi')}</option>
                <option value="agent">ssh-agent / Pageant</option>
              </select>
            </label>
            {kind === 'password' && (
              <label>
                <span className="mb-0.5 block text-xs text-neutral-400">
                  {initial ? t('dialogs.passwordLabelEdit') : t('dialogs.password')}
                </span>
                <input
                  className={input}
                  type="password"
                  value={password}
                  autoComplete={initial ? 'current-password' : 'new-password'}
                  spellCheck={false}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </label>
            )}
            {kind === 'publicKey' && (
              <>
                <label>
                  <span className="mb-0.5 block text-xs text-neutral-400">
                    {t('dialogs.keyPath')}
                  </span>
                  <input
                    className={input}
                    value={keyPath}
                    autoComplete="off"
                    spellCheck={false}
                    onChange={(e) => setKeyPath(e.target.value)}
                    placeholder={t('dialogs.keyPathPlaceholder')}
                  />
                </label>
                <label>
                  <span className="mb-0.5 block text-xs text-neutral-400">
                    {t('dialogs.passphraseLabel')}
                  </span>
                  <input
                    className={input}
                    type="password"
                    value={passphrase}
                    spellCheck={false}
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
            <p className="py-6 text-center text-xs text-neutral-500">{t('dialogs.jumpNeedSave')}</p>
          ))}

        {tab === 'tunnels' &&
          (initial ? (
            <SessionTunnelsTab sessionId={initial.id} />
          ) : (
            <p className="py-6 text-center text-xs text-neutral-500">
              {t('dialogs.tunnelsNeedSave')}
            </p>
          ))}

        {error && (
          <p aria-live="polite" className="text-xs text-red-400">
            {error}
          </p>
        )}
      </div>

      <div className="mt-2 flex items-center justify-between gap-2">
        <div className="flex min-w-0 items-center gap-2">
          {sessKind === 'ssh' && (
            <button
              className="shrink-0 rounded border border-neutral-700 px-3 py-1 text-neutral-300 hover:bg-neutral-800 disabled:opacity-50"
              onClick={() => void testConnect()}
              disabled={busy || test.phase === 'testing' || !host.trim() || !user.trim()}
            >
              {test.phase === 'testing' ? t('dialogs.testing') : t('dialogs.testConnect')}
            </button>
          )}
          {test.phase === 'ok' && (
            <span aria-live="polite" className="truncate text-xs text-green-400">
              {test.latencyMs !== null
                ? t('dialogs.testOkLatency', { ms: test.latencyMs })
                : t('dialogs.testOk')}
            </span>
          )}
          {test.phase === 'err' && (
            <span aria-live="polite" className="truncate text-xs text-red-400" title={test.message}>
              ✗ {test.message}
            </span>
          )}
        </div>
        <div className="flex shrink-0 gap-2">
          <button
            className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
            onClick={close}
          >
            {t('dialogs.cancel')}
          </button>
          {sessKind === 'ssh' && save && (
            <button
              className="rounded border border-neutral-700 px-3 py-1 text-neutral-300 hover:bg-neutral-800 disabled:opacity-50"
              onClick={() => void submit(false)}
              disabled={busy || !host.trim() || !user.trim()}
            >
              {t('dialogs.saveOnly')}
            </button>
          )}
          <button
            className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500 disabled:opacity-50"
            onClick={() => void submit(true)}
            disabled={busy || (sessKind === 'ssh' && (!host.trim() || !user.trim()))}
          >
            {sessKind === 'local'
              ? t('dialogs.saveAndOpen')
              : save
                ? t('dialogs.saveAndConnect')
                : t('dialogs.connect')}
          </button>
        </div>
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
  const t = useT();

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
      notify(t('dialogs.tunnelStartFailed', { error: String(e) }), 'error');
    }
  };

  return (
    <div className="text-xs">
      <div className="mb-1 flex items-center justify-between">
        <span className="text-neutral-500">{t('dialogs.tunnelsBoundOnly')}</span>
        <button
          className="rounded border border-neutral-700 px-2 py-0.5 hover:bg-neutral-800"
          onClick={() => setEditor({ def: null })}
        >
          {t('dialogs.addTunnel')}
        </button>
      </div>
      {defs.length === 0 ? (
        <p className="py-4 text-center text-neutral-600">{t('dialogs.noTunnels')}</p>
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
                  {rt ? rt.status : t('dialogs.notRunning')}
                </span>
                {rt ? (
                  <button
                    className="rounded px-1 text-neutral-500 hover:text-red-400"
                    onClick={() => void stopTunnel(d.id)}
                  >
                    {t('dialogs.stop')}
                  </button>
                ) : (
                  <button
                    className="rounded px-1 text-neutral-500 hover:text-green-400"
                    onClick={() => void startDef(d)}
                  >
                    {t('dialogs.start')}
                  </button>
                )}
                <button
                  className="rounded px-1 text-neutral-500 hover:text-neutral-200"
                  onClick={() => setEditor({ def: d })}
                >
                  {t('dialogs.edit')}
                </button>
                <button
                  className="rounded px-1 text-neutral-500 hover:text-neutral-200"
                  onClick={() => void duplicateTunnel(d)}
                >
                  {t('dialogs.duplicate')}
                </button>
                <button
                  className="rounded px-1 text-neutral-500 hover:text-red-400"
                  onClick={() => setPendingDelete(d)}
                >
                  {t('dialogs.delete')}
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
          title={t('dialogs.deleteTunnelTitle', { name: tunnelDisplayName(pendingDelete) })}
          confirmLabel={t('dialogs.delete')}
          onConfirm={() => {
            void deleteTunnel(pendingDelete.id)
              .then(() => notify(t('dialogs.tunnelDeleted'), 'success'))
              .catch((e) => notify(t('dialogs.tunnelDeleteFailed', { error: String(e) }), 'error'));
            setPendingDelete(null);
          }}
          onCancel={() => setPendingDelete(null)}
        >
          {t('dialogs.deleteTunnelBody', {
            spec: `${pendingDelete.bindHost}:${pendingDelete.bindPort}${
              pendingDelete.targetHost
                ? ` → ${pendingDelete.targetHost}:${pendingDelete.targetPort}`
                : ''
            }`,
          })}
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
  const candidates = sessions.filter(
    (s) => s.id !== excludeId && !chain.includes(s.id) && s.kind !== 'local',
  );
  const nameOf = (id: string) => sessions.find((s) => s.id === id)?.name ?? id;
  const t = useT();

  return (
    <div className="mt-1 rounded border border-neutral-800 p-2">
      <div className="mb-1 text-xs text-neutral-400">{t('dialogs.jumpChainLabel')}</div>
      {chain.map((id, i) => (
        <div key={id} className="mb-1 flex items-center gap-2 text-xs">
          <span className="text-neutral-500">{i + 1}.</span>
          <span className="flex-1 truncate">{nameOf(id)}</span>
          <button
            className="text-neutral-500 hover:text-red-400"
            onClick={() => onChange(chain.filter((x) => x !== id))}
          >
            {t('dialogs.remove')}
          </button>
        </div>
      ))}
      {candidates.length > 0 && (
        <select
          className="w-full rounded border border-neutral-700 bg-neutral-800 px-1.5 py-1 text-xs"
          value=""
          onChange={(e) => e.target.value && onChange([...chain, e.target.value])}
        >
          <option value="">{t('dialogs.addJump')}</option>
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
