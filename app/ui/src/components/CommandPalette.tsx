import { useMemo, useRef, useState } from 'react';

import { fuzzyMatchAny } from '../term/fuzzy';
import { KEY_ACTIONS, keymapFromSettings } from '../term/keymap';
import { useAppStore } from '../state/app-store';

/**
 * 全局命令面板（Ctrl+Shift+P）：会话模糊检索直连 + 应用动作。
 * ↑↓ 选择，Enter 执行，Esc 关闭。导出加密/配置导入需二级输入（口令/路径）。
 */

interface Action {
  id: string;
  label: string;
  hint?: string;
  /** 关联 keymap 动作 id（有则在列表项展示生效快捷键） */
  keyId?: string;
  /** 需要二级输入时的提示语；不缺省即直接执行 */
  input?: { placeholder: string; secret?: boolean };
  run: (input?: string) => void | Promise<void>;
}

/** 动作项的快捷键展示：生效主绑定 + 固定别名；无 keyId 落回「命令」 */
function shortcutOf(a: Action, settings: Record<string, unknown>): string {
  if (!a.keyId) return '命令';
  const b = keymapFromSettings(settings)[a.keyId];
  if (!b) return '命令';
  const alias = KEY_ACTIONS.find((x) => x.id === a.keyId)?.alias;
  return b + (alias ? ` / ${alias}` : '');
}

/** 由外层 {paletteOpen && <CommandPalette/>} 控制挂载——重挂载即重置态，无需 effect 重置 */
export function CommandPalette() {
  const toggle = useAppStore((s) => s.togglePalette);
  const sessions = useAppStore((s) => s.sessions);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const openConnect = useAppStore((s) => s.openConnect);
  const toggleTunnelPanel = useAppStore((s) => s.toggleTunnelPanel);
  const toggleSidebar = useAppStore((s) => s.toggleSidebar);
  const importFrom = useAppStore((s) => s.importFrom);
  const exportConfig = useAppStore((s) => s.exportConfig);
  const importConfigFile = useAppStore((s) => s.importConfigFile);
  const toggleSftpActive = () => {
    const s = useAppStore.getState();
    if (s.activeId) s.toggleSftp(s.activeId);
  };
  const toggleMetricsActive = () => {
    const s = useAppStore.getState();
    if (s.activeId) s.toggleMetrics(s.activeId);
  };

  const [query, setQuery] = useState('');
  const settings = useAppStore((s) => s.settings);
  const [index, setIndex] = useState(0);
  /** 非空 = 二级输入态（对 pendingAction 收集口令/路径） */
  const [pending, setPending] = useState<Action | null>(null);
  const [subInput, setSubInput] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);

  const actions = useMemo<Action[]>(
    () => [
      { id: 'a-new', label: '新建会话', keyId: 'newTab', run: () => openConnect() },
      { id: 'a-tunnel', label: '隧道面板', keyId: 'tunnels', run: () => toggleTunnelPanel() },
      {
        id: 'a-sftp',
        label: 'SFTP 面板开关（当前标签）',
        keyId: 'sftp',
        run: () => toggleSftpActive(),
      },
      {
        id: 'a-metrics',
        label: '监控面板开关（当前标签）',
        keyId: 'metrics',
        run: () => toggleMetricsActive(),
      },
      {
        id: 'a-settings',
        label: '设置',
        keyId: 'settings',
        run: () => useAppStore.getState().toggleSettings(),
      },
      { id: 'a-sidebar', label: '侧栏开关', run: () => toggleSidebar() },
      {
        id: 'a-imp-ssh',
        label: '导入 OpenSSH 配置（~/.ssh/config）',
        run: () => importFrom('openssh'),
      },
      { id: 'a-imp-putty', label: '导入 PuTTY 会话（注册表）', run: () => importFrom('putty') },
      { id: 'a-imp-xs', label: '导入 Xshell 会话', run: () => importFrom('xshell') },
      { id: 'a-imp-fs', label: '导入 FinalShell 会话', run: () => importFrom('finalshell') },
      { id: 'a-exp-plain', label: '导出配置（明文，不含凭据）', run: () => exportConfig(false) },
      {
        id: 'a-exp-enc',
        label: '导出配置（加密，含凭据）',
        input: { placeholder: '导出口令（导入时需同一口令）', secret: true },
        run: (p) => exportConfig(true, p),
      },
      {
        id: 'a-imp-cfg',
        label: '导入配置文件（myssh-config-*.json）',
        input: { placeholder: '配置文件完整路径' },
        run: (p) => importConfigFile(p ?? ''),
      },
    ],
    [openConnect, toggleTunnelPanel, toggleSidebar, importFrom, exportConfig, importConfigFile],
  );

  type Item =
    | { kind: 'session'; id: string; label: string; hint: string; score: number }
    | { kind: 'action'; action: Action; score: number };

  const items = useMemo<Item[]>(() => {
    const out: Item[] = [];
    for (const s of sessions) {
      const score = fuzzyMatchAny(query, [s.name, s.host, s.username, s.groupPath, ...s.tags]);
      if (score !== null)
        out.push({
          kind: 'session',
          id: s.id,
          label: s.name,
          hint: `${s.username}@${s.host}:${s.port}`,
          score: score + 100_000, // 会话优先于动作
        });
    }
    for (const a of actions) {
      const score = fuzzyMatchAny(query, [a.label]);
      if (score !== null) out.push({ kind: 'action', action: a, score });
    }
    out.sort((x, y) => y.score - x.score);
    return out.slice(0, 12);
  }, [query, sessions, actions]);

  const execute = (item: Item) => {
    if (item.kind === 'session') {
      const s = sessions.find((x) => x.id === item.id);
      if (s) connectBySession(s.id, s.name);
      toggle();
      return;
    }
    const a = item.action;
    if (a.input) {
      setPending(a);
      setSubInput('');
      return;
    }
    toggle();
    void a.run();
  };

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      if (pending) setPending(null);
      else toggle();
      return;
    }
    if (pending) {
      if (e.key === 'Enter') {
        const a = pending;
        toggle();
        void a.run(subInput);
      }
      return;
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setIndex((i) => Math.min(i + 1, items.length - 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === 'Enter' && items[index]) {
      execute(items[index]);
    }
  };

  return (
    <div
      className="fixed inset-0 z-30 flex items-start justify-center bg-black/50 pt-24"
      role="dialog"
      aria-modal="true"
      aria-label="命令面板"
      onClick={toggle}
    >
      <div
        className="w-[34rem] overflow-hidden rounded-lg border border-neutral-700 bg-neutral-900 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {pending ? (
          <input
            ref={inputRef}
            autoFocus
            className="w-full border-b border-neutral-800 bg-neutral-900 px-4 py-3 text-sm text-neutral-200 outline-none"
            placeholder={pending.input?.placeholder}
            type={pending.input?.secret ? 'password' : 'text'}
            value={subInput}
            onChange={(e) => setSubInput(e.target.value)}
            onKeyDown={onKeyDown}
          />
        ) : (
          <input
            ref={inputRef}
            autoFocus
            className="w-full border-b border-neutral-800 bg-neutral-900 px-4 py-3 text-sm text-neutral-200 outline-none"
            placeholder="检索会话或命令…"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setIndex(0);
            }}
            onKeyDown={onKeyDown}
          />
        )}
        {!pending && (
          <ul className="max-h-80 overflow-y-auto py-1">
            {items.length === 0 && <li className="px-4 py-3 text-xs text-neutral-600">无匹配</li>}
            {items.map((item, i) => (
              <li key={item.kind === 'session' ? item.id : item.action.id}>
                <button
                  className={`flex w-full items-center justify-between px-4 py-2 text-left text-sm ${
                    i === index ? 'bg-blue-600/30 text-neutral-100' : 'text-neutral-300'
                  }`}
                  onMouseEnter={() => setIndex(i)}
                  onClick={() => execute(item)}
                >
                  <span>{item.kind === 'session' ? item.label : item.action.label}</span>
                  <span className="text-xs text-neutral-500">
                    {item.kind === 'session' ? item.hint : shortcutOf(item.action, settings)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
    </div>
  );
}
