import { useMemo, useRef, useState } from 'react';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';

import { fuzzyMatchAny } from '../term/fuzzy';
import { KEY_ACTIONS, keymapFromSettings } from '../term/keymap';
import { reconnectRegistry } from '../term/registry';
import { BUILTIN_THEMES } from '../term/themes';
import { useAppStore } from '../state/app-store';
import { GROUP_KEYS, readStringList, sshCommand } from '../state/groups';

/**
 * 全局命令面板（Ctrl+Shift+P）：会话/分组/动作三类分区检索。
 * ↑↓ 选择，Enter 执行，Esc 关闭。导出加密/配置导入需二级输入（口令/路径）。
 * 12.4：pane/标签/服务器级动作扩展；动作项右侧展示生效快捷键。
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

/** 取活跃标签与其活跃 pane；无则 null（动作守卫共用） */
function activeTabPane() {
  const s = useAppStore.getState();
  const tab = s.tabs.find((t) => t.id === s.activeId);
  if (!tab) return null;
  return { s, tab, paneId: tab.activePaneId };
}

/** 活跃标签的会话档案 id（session 目标才有） */
function activeSessionId(): string | null {
  const ctx = activeTabPane();
  return ctx && ctx.tab.target.kind === 'session' ? ctx.tab.target.sessionId : null;
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
      {
        id: 'a-reconnect-pane',
        label: '重新连接当前 pane',
        run: () => {
          const ctx = activeTabPane();
          if (ctx) reconnectRegistry.get(ctx.paneId)?.();
        },
      },
      {
        id: 'a-disconnect-pane',
        label: '断开当前连接',
        run: () => {
          const ctx = activeTabPane();
          if (ctx) ctx.s.disconnectPane(ctx.tab.id, ctx.paneId);
        },
      },
      {
        id: 'a-close-pane',
        label: '关闭当前 pane',
        run: () => {
          const ctx = activeTabPane();
          if (ctx) ctx.s.closePane(ctx.tab.id, ctx.paneId);
        },
      },
      {
        id: 'a-close-tab',
        label: '关闭当前标签',
        keyId: 'closeTab',
        run: () => {
          const s = useAppStore.getState();
          if (s.activeId) s.closeTab(s.activeId);
        },
      },
      {
        id: 'a-close-other-tabs',
        label: '关闭其他标签',
        run: () => {
          const s = useAppStore.getState();
          if (s.activeId) s.closeOtherTabs(s.activeId);
        },
      },
      {
        id: 'a-split-row',
        label: '向右分屏',
        keyId: 'splitRow',
        run: () => useAppStore.getState().splitActive('row'),
      },
      {
        id: 'a-split-col',
        label: '向下分屏',
        keyId: 'splitCol',
        run: () => useAppStore.getState().splitActive('col'),
      },
      {
        id: 'a-sftp',
        label: '打开当前服务器 SFTP',
        keyId: 'sftp',
        run: () => toggleSftpActive(),
      },
      {
        id: 'a-metrics',
        label: '打开当前服务器监控',
        keyId: 'metrics',
        run: () => toggleMetricsActive(),
      },
      {
        id: 'a-tunnel',
        label: '管理当前服务器隧道',
        keyId: 'tunnels',
        run: () => toggleTunnelPanel(),
      },
      {
        id: 'a-favorite',
        label: '收藏当前服务器（切换）',
        run: () => {
          const sid = activeSessionId();
          if (sid) useAppStore.getState().toggleFavorite(sid);
          else useAppStore.getState().notify('当前标签不是服务器档案连接', 'warning');
        },
      },
      {
        id: 'a-detach',
        label: '分离窗口（当前服务器）',
        run: () => {
          const sid = activeSessionId();
          const ctx = activeTabPane();
          if (sid && ctx) ctx.s.connectInNewWindow(sid, ctx.tab.title);
          else useAppStore.getState().notify('当前标签不是服务器档案连接', 'warning');
        },
      },
      {
        id: 'a-copy-ssh',
        label: '复制 SSH 命令（当前服务器）',
        run: () => {
          const sid = activeSessionId();
          const s = useAppStore.getState();
          const rec = sid ? s.sessions.find((x) => x.id === sid) : undefined;
          if (!rec) {
            s.notify('当前标签不是服务器档案连接', 'warning');
            return;
          }
          void writeText(sshCommand(rec, s.sessions)).then(
            () => s.notify('SSH 命令已复制', 'success'),
            (e: unknown) => s.notify(`复制失败: ${String(e)}`, 'error'),
          );
        },
      },
      {
        id: 'a-theme',
        label: '切换主题（循环内置主题）',
        run: () => {
          const s = useAppStore.getState();
          const cur = typeof s.settings['theme'] === 'string' ? s.settings['theme'] : 'one-dark';
          const idx = BUILTIN_THEMES.findIndex((t) => t.id === cur);
          const next = BUILTIN_THEMES[(idx + 1) % BUILTIN_THEMES.length];
          s.setSetting('theme', next.id);
          s.notify(`主题：${next.label}`, 'success');
        },
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
    | { kind: 'group'; path: string; score: number }
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
          score,
        });
    }
    // 分组（12.4 分区）：唯一非空 groupPath，选中即打开侧栏并展开
    const groups = [...new Set(sessions.map((s) => s.groupPath).filter((g) => g !== ''))];
    for (const g of groups) {
      const score = fuzzyMatchAny(query, [g]);
      if (score !== null) out.push({ kind: 'group', path: g, score });
    }
    for (const a of actions) {
      const score = fuzzyMatchAny(query, [a.label]);
      if (score !== null) out.push({ kind: 'action', action: a, score });
    }
    // 分区内按分数排序；区间固定 服务器 → 分组 → 操作
    const rank = (i: Item) => (i.kind === 'session' ? 0 : i.kind === 'group' ? 1 : 2);
    out.sort((x, y) => rank(x) - rank(y) || y.score - x.score);
    return out.slice(0, 30);
  }, [query, sessions, actions]);

  const execute = (item: Item) => {
    if (item.kind === 'session') {
      const s = sessions.find((x) => x.id === item.id);
      if (s) connectBySession(s.id, s.name);
      toggle();
      return;
    }
    if (item.kind === 'group') {
      const s = useAppStore.getState();
      if (!s.sidebarOpen) s.toggleSidebar();
      const collapsed = readStringList(s.settings[GROUP_KEYS.collapsed]);
      s.setGroupList(
        'groups.collapsed',
        collapsed.filter((g) => g !== item.path && !g.startsWith(item.path + '/')),
      );
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

  /** 分区标题（item 与上一个 kind 不同即插入） */
  const sectionOf = (item: Item): string | null =>
    item.kind === 'session' ? '服务器' : item.kind === 'group' ? '分组' : '操作';

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
            placeholder="检索会话、分组或命令…"
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
              <li
                key={
                  item.kind === 'action'
                    ? item.action.id
                    : item.kind === 'group'
                      ? `g:${item.path}`
                      : item.id
                }
              >
                {(i === 0 || sectionOf(items[i - 1] ?? item) !== sectionOf(item)) && (
                  <div className="px-4 pt-1.5 pb-0.5 text-[10px] text-neutral-600">
                    {sectionOf(item)}
                  </div>
                )}
                <button
                  className={`flex w-full items-center justify-between px-4 py-2 text-left text-sm ${
                    i === index ? 'bg-blue-600/30 text-neutral-100' : 'text-neutral-300'
                  }`}
                  onMouseEnter={() => setIndex(i)}
                  onClick={() => execute(item)}
                >
                  <span>
                    {item.kind === 'session'
                      ? item.label
                      : item.kind === 'group'
                        ? `📁 ${item.path}`
                        : item.action.label}
                  </span>
                  <span className="text-xs text-neutral-500">
                    {item.kind === 'session'
                      ? item.hint
                      : item.kind === 'group'
                        ? '分组'
                        : shortcutOf(item.action, settings)}
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
