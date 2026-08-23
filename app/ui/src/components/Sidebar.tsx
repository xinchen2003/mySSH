import { useEffect, useMemo, useState } from 'react';
import { useAppStore } from '../state/app-store';
import { fuzzyMatchAny } from '../term/fuzzy';
import type { SessionRecord } from '../term/types';

/**
 * 会话档案侧栏：groupPath 多级分组树（原生 details 折叠）+ 模糊搜索 +
 * 点击直连。导入/导出入口在顶部 ⋯ 菜单。
 */
export function Sidebar() {
  const sessions = useAppStore((s) => s.sessions);
  const open = useAppStore((s) => s.sidebarOpen);
  const load = useAppStore((s) => s.loadSessions);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const deleteSession = useAppStore((s) => s.deleteSession);
  const openConnect = useAppStore((s) => s.openConnect);
  const importFrom = useAppStore((s) => s.importFrom);
  const exportConfig = useAppStore((s) => s.exportConfig);
  const importConfigFile = useAppStore((s) => s.importConfigFile);

  const [query, setQuery] = useState('');
  const [menuOpen, setMenuOpen] = useState(false);
  /** 小弹层：导出口令 / 导入路径 */
  const [prompt, setPrompt] = useState<
    | { mode: 'export-enc'; input: string }
    | { mode: 'import-cfg'; input: string; pass: string }
    | null
  >(null);

  useEffect(() => {
    void load();
  }, [load]);

  const filtered = useMemo(() => {
    if (!query.trim()) return sessions;
    return sessions
      .map((s) => ({
        s,
        score: fuzzyMatchAny(query, [s.name, s.host, s.username, s.groupPath, ...s.tags]),
      }))
      .filter((x): x is { s: SessionRecord; score: number } => x.score !== null)
      .sort((a, b) => b.score - a.score)
      .map((x) => x.s);
  }, [sessions, query]);

  // 分组树：groupPath "a/b/c" → 嵌套节点；'' 归「未分组」
  const tree = useMemo(() => buildGroupTree(filtered), [filtered]);

  if (!open) return null;

  const menuItem = (label: string, fn: () => void) => (
    <button
      className="block w-full px-3 py-1.5 text-left text-xs text-neutral-300 hover:bg-neutral-800"
      onClick={() => {
        setMenuOpen(false);
        fn();
      }}
    >
      {label}
    </button>
  );

  return (
    <aside className="flex w-56 shrink-0 flex-col border-r border-neutral-800 bg-neutral-900">
      <div className="relative flex items-center justify-between px-3 py-2 text-xs text-neutral-400">
        <span>会话</span>
        <span className="flex gap-1">
          <button
            className="rounded px-1 hover:bg-neutral-800"
            title="导入/导出"
            onClick={() => setMenuOpen((v) => !v)}
          >
            ⋯
          </button>
          <button
            className="rounded px-1 hover:bg-neutral-800"
            title="新建会话"
            onClick={() => openConnect()}
          >
            ＋
          </button>
        </span>
        {menuOpen && (
          <div className="absolute right-1 top-7 z-20 w-44 rounded border border-neutral-700 bg-neutral-900 py-1 shadow-xl">
            {menuItem('导入 OpenSSH 配置', () => void importFrom('openssh'))}
            {menuItem('导入 PuTTY（注册表）', () => void importFrom('putty'))}
            {menuItem('导入 Xshell', () => void importFrom('xshell'))}
            {menuItem('导入 FinalShell', () => void importFrom('finalshell'))}
            <div className="my-1 border-t border-neutral-800" />
            {menuItem('导出配置（明文）', () => void exportConfig(false))}
            {menuItem('导出配置（加密）…', () => setPrompt({ mode: 'export-enc', input: '' }))}
            {menuItem('导入配置文件…', () =>
              setPrompt({ mode: 'import-cfg', input: '', pass: '' }),
            )}
          </div>
        )}
      </div>

      <div className="px-2 pb-1">
        <input
          className="w-full rounded border border-neutral-800 bg-neutral-950 px-2 py-1 text-xs text-neutral-300 outline-none placeholder:text-neutral-600 focus:border-neutral-600"
          placeholder="搜索会话…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>

      <div className="flex-1 overflow-y-auto px-1 pb-2">
        {sessions.length === 0 && (
          <p className="px-2 py-4 text-center text-xs text-neutral-600">
            还没有会话。点右上角 ＋ 新建，或 ⋯ 菜单导入。
          </p>
        )}
        {query.trim() ? (
          // 搜索态：平铺结果（分组树让位匹配列表）
          filtered.map((s) => (
            <SessionRow
              key={s.id}
              s={s}
              onConnect={() => connectBySession(s.id, s.name)}
              onEdit={() => openConnect(s)}
              onDelete={() => void deleteSession(s.id)}
            />
          ))
        ) : (
          <GroupNode
            node={tree}
            depth={0}
            onConnect={connectBySession}
            onEdit={openConnect}
            onDelete={deleteSession}
          />
        )}
      </div>

      {prompt && (
        <div className="absolute inset-0 z-30 flex items-center justify-center bg-black/60">
          <div className="w-72 rounded-lg border border-neutral-700 bg-neutral-900 p-3 text-xs text-neutral-200 shadow-xl">
            {prompt.mode === 'export-enc' ? (
              <>
                <div className="mb-2">导出口令（导入时需同一口令）</div>
                <input
                  type="password"
                  className="mb-2 w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  value={prompt.input}
                  onChange={(e) => setPrompt({ mode: 'export-enc', input: e.target.value })}
                  autoFocus
                />
              </>
            ) : (
              <>
                <div className="mb-2">配置文件完整路径</div>
                <input
                  className="mb-2 w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  placeholder="…/myssh-config-*.json"
                  value={prompt.input}
                  onChange={(e) => setPrompt({ ...prompt, input: e.target.value })}
                  autoFocus
                />
                <input
                  type="password"
                  className="mb-2 w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  placeholder="口令（加密包络时必填）"
                  value={prompt.pass}
                  onChange={(e) => setPrompt({ ...prompt, pass: e.target.value })}
                />
              </>
            )}
            <div className="flex justify-end gap-2">
              <button
                className="rounded px-2 py-1 text-neutral-400 hover:bg-neutral-800"
                onClick={() => setPrompt(null)}
              >
                取消
              </button>
              <button
                className="rounded bg-blue-600 px-2 py-1 text-white hover:bg-blue-500"
                onClick={() => {
                  const p = prompt;
                  setPrompt(null);
                  if (p.mode === 'export-enc') void exportConfig(true, p.input);
                  else void importConfigFile(p.input, p.pass || undefined);
                }}
              >
                确定
              </button>
            </div>
          </div>
        </div>
      )}
    </aside>
  );
}

// ---------- 分组树 ----------

interface GroupTree {
  /** 子分组：名 → 子树 */
  groups: Map<string, GroupTree>;
  /** 本层直属会话 */
  items: SessionRecord[];
}

function emptyTree(): GroupTree {
  return { groups: new Map(), items: [] };
}

function buildGroupTree(sessions: SessionRecord[]): GroupTree {
  const root = emptyTree();
  for (const s of sessions) {
    const parts = (s.groupPath || '未分组').split('/').filter(Boolean);
    let node = root;
    for (const p of parts) {
      let child = node.groups.get(p);
      if (!child) {
        child = emptyTree();
        node.groups.set(p, child);
      }
      node = child;
    }
    node.items.push(s);
  }
  return root;
}

function GroupNode({
  node,
  depth,
  onConnect,
  onEdit,
  onDelete,
}: {
  node: GroupTree;
  depth: number;
  onConnect: (id: string, title: string) => void;
  onEdit: (s: SessionRecord) => void;
  onDelete: (id: string) => Promise<void>;
}) {
  return (
    <>
      {node.items.map((s) => (
        <SessionRow
          key={s.id}
          s={s}
          indent={depth}
          onConnect={() => onConnect(s.id, s.name)}
          onEdit={() => onEdit(s)}
          onDelete={() => void onDelete(s.id)}
        />
      ))}
      {[...node.groups.entries()].map(([name, child]) => (
        <details key={name} open className="mt-0.5">
          <summary
            className="cursor-pointer select-none rounded px-1 py-0.5 text-xs text-neutral-500 hover:bg-neutral-800"
            style={{ paddingLeft: `${depth * 12 + 4}px` }}
          >
            {name}
          </summary>
          <GroupNode
            node={child}
            depth={depth + 1}
            onConnect={onConnect}
            onEdit={onEdit}
            onDelete={onDelete}
          />
        </details>
      ))}
    </>
  );
}

function SessionRow({
  s,
  indent = 0,
  onConnect,
  onEdit,
  onDelete,
}: {
  s: SessionRecord;
  indent?: number;
  onConnect: () => void;
  onEdit: () => void;
  onDelete: () => void;
}) {
  return (
    <div
      className="group flex items-center rounded px-1 py-1 hover:bg-neutral-800"
      style={{ paddingLeft: `${indent * 12 + 8}px` }}
    >
      <button
        className="min-w-0 flex-1 text-left"
        onClick={onConnect}
        title={`${s.username}@${s.host}:${s.port}`}
      >
        <div className="truncate text-sm text-neutral-200">{s.name}</div>
        <div className="truncate text-xs text-neutral-500">
          {s.username}@{s.host}:{s.port}
          {s.jumpChain.length > 0 && ` · ${s.jumpChain.length} 跳`}
        </div>
      </button>
      <span className="hidden shrink-0 gap-1 group-hover:flex">
        <button
          className="rounded px-1 text-xs text-neutral-500 hover:text-neutral-200"
          onClick={onEdit}
        >
          编辑
        </button>
        <button
          className="rounded px-1 text-xs text-neutral-500 hover:text-red-400"
          onClick={onDelete}
        >
          删
        </button>
      </span>
    </div>
  );
}
