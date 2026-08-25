import { useEffect, useMemo, useState } from 'react';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import { fuzzyMatchAny } from '../term/fuzzy';
import type { SessionRecord } from '../term/types';
import { ContextMenu, type MenuItem } from './ContextMenu';
import { ConfirmDialog } from './ConfirmDialog';
import { Dialog } from './Dialog';
import {
  buildGroupTree,
  canMoveGroup,
  collectGroupPaths,
  filterVirtual,
  flattenVisible,
  isValidGroupPath,
  rangeSessionIds,
  readFailedList,
  readStringList,
  rewritePathOnDeleteKeep,
  rewritePathOnRename,
  sshCommand,
  GROUP_KEYS,
  type VirtualView,
} from '../state/groups';

/** 连接目标语义：'connect' 复用已有标签，'new-tab' 强制新标签 */
type ConnectMode = 'connect' | 'new-tab';

/** 侧栏小弹层（导入导出口令/路径、分组新建/重命名、移动到分组） */
type Prompt =
  | { mode: 'export-enc'; input: string }
  | { mode: 'import-cfg'; input: string; pass: string }
  | { mode: 'group-new'; parent: string; input: string }
  | { mode: 'group-rename'; path: string; input: string }
  | { mode: 'move-to'; ids: string[]; input: string };

const VIRTUAL_LABELS: { id: VirtualView; label: string }[] = [
  { id: 'favorites', label: '收藏' },
  { id: 'recent', label: '最近连接' },
  { id: 'online', label: '当前在线' },
  { id: 'ungrouped', label: '未分组' },
  { id: 'failed', label: '最近失败' },
];

/**
 * 服务器库侧栏（批次二）：
 * 单击选中/双击连接/Enter 连接（设置可恢复单击直连）；右键菜单；Ctrl/Shift 多选与批量操作；
 * 分组树（折叠持久化、计数、拖拽移动、增删改）；虚拟分组（收藏/最近/在线/未分组/失败）。
 */
export function Sidebar() {
  const sessions = useAppStore((s) => s.sessions);
  const open = useAppStore((s) => s.sidebarOpen);
  const load = useAppStore((s) => s.loadSessions);
  const settings = useAppStore((s) => s.settings);
  const tabs = useAppStore((s) => s.tabs);
  const openConnect = useAppStore((s) => s.openConnect);
  const importFrom = useAppStore((s) => s.importFrom);
  const exportConfig = useAppStore((s) => s.exportConfig);
  const importConfigFile = useAppStore((s) => s.importConfigFile);

  const [query, setQuery] = useState('');
  const [menuOpen, setMenuOpen] = useState(false);
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  /** 多选：anchor 为 Shift 连选基准 */
  const [sel, setSel] = useState<{ anchor: string | null; ids: string[] }>({
    anchor: null,
    ids: [],
  });
  /** 右键菜单 */
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  /** 非空分组删除三选 */
  const [groupDel, setGroupDel] = useState<{ path: string; count: number } | null>(null);
  /** 级联删除（分组+服务器）的二次确认 */
  const [confirmCascade, setConfirmCascade] = useState<{ path: string; count: number } | null>(
    null,
  );
  /** 批量删除确认 */
  const [batchDel, setBatchDel] = useState<SessionRecord[] | null>(null);
  /** 虚拟分组视图 */
  const [virt, setVirt] = useState<VirtualView | null>(null);
  /** 拖拽悬停目标（分组路径；'' = 根/未分组） */
  const [dropTarget, setDropTarget] = useState<string | null>(null);

  useEffect(() => {
    void load();
  }, [load]);

  const favorites = useMemo(
    () => new Set(readStringList(settings[GROUP_KEYS.favorites])),
    [settings],
  );
  const recent = useMemo(() => readStringList(settings[GROUP_KEYS.recent]), [settings]);
  const failed = useMemo(
    () => new Map(readFailedList(settings[GROUP_KEYS.failed]).map((f) => [f.id, f.message])),
    [settings],
  );
  const collapsed = useMemo(
    () => new Set(readStringList(settings[GROUP_KEYS.collapsed])),
    [settings],
  );
  const extras = useMemo(() => readStringList(settings[GROUP_KEYS.extraGroups]), [settings]);
  const online = useMemo(() => {
    const set = new Set<string>();
    for (const t of tabs) {
      if (t.target.kind !== 'session') continue;
      const live = Object.values(t.panes).some(
        (p) => p.state === 'connected' || p.state === 'connecting' || p.state === 'reconnecting',
      );
      if (live) set.add(t.target.sessionId);
    }
    return set;
  }, [tabs]);

  const clickToConnect = settings[GROUP_KEYS.clickToConnect] === true;

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

  const tree = useMemo(() => buildGroupTree(filtered, extras), [filtered, extras]);
  const flatMode = query.trim() !== '' || virt !== null;
  const virtList = useMemo(
    () => (virt ? filterVirtual(virt, sessions, { favorites, recent, online, failed }) : []),
    [virt, sessions, favorites, recent, online, failed],
  );
  const visibleRows = useMemo(() => flattenVisible(tree, collapsed), [tree, collapsed]);
  /** 当前模式下的可见会话 id 序（Shift 连选区间基于此） */
  const visibleSessionIds = useMemo(
    () =>
      flatMode
        ? (virt ? virtList : filtered).map((s) => s.id)
        : visibleRows.filter((r) => r.kind === 'session').map((r) => r.key),
    [flatMode, virt, virtList, filtered, visibleRows],
  );

  if (!open) return null;

  const s = () => useAppStore.getState();

  // ---------- 选择 ----------

  const clickRow = (rec: SessionRecord, e: React.MouseEvent) => {
    if (clickToConnect && !e.ctrlKey && !e.metaKey && !e.shiftKey) {
      s().connectOrActivate(rec.id, rec.name);
      return;
    }
    if (e.shiftKey && sel.anchor) {
      setSel({ anchor: sel.anchor, ids: rangeSessionIds(visibleSessionIds, sel.anchor, rec.id) });
    } else if (e.ctrlKey || e.metaKey) {
      const has = sel.ids.includes(rec.id);
      setSel({
        anchor: rec.id,
        ids: has ? sel.ids.filter((x) => x !== rec.id) : [...sel.ids, rec.id],
      });
    } else {
      setSel({ anchor: rec.id, ids: [rec.id] });
    }
  };

  const selectForMenu = (rec: SessionRecord) => {
    if (!sel.ids.includes(rec.id)) setSel({ anchor: rec.id, ids: [rec.id] });
  };

  // ---------- 连接 ----------

  const connect = (rec: SessionRecord, mode: ConnectMode = 'connect') => {
    if (mode === 'new-tab') s().connectBySession(rec.id, rec.name);
    else s().connectOrActivate(rec.id, rec.name);
  };

  // ---------- 分组操作 ----------

  const persistExtras = (list: string[]) => s().setGroupList('groups.extra', list);
  const persistCollapsed = (list: string[]) => s().setGroupList('groups.collapsed', list);

  const createGroup = (parent: string, name: string) => {
    const seg = name.trim().replace(/\//g, '');
    if (!seg) return;
    const path = parent ? `${parent}/${seg}` : seg;
    if (extras.includes(path) || collectGroupPaths(tree).includes(path)) {
      s().notify(`分组已存在: ${path}`, 'warning');
      return;
    }
    persistExtras([...extras, path]);
    if (parent && collapsed.has(parent))
      persistCollapsed([...collapsed].filter((p) => p !== parent));
    s().notify(`已创建分组 ${path}`, 'success');
  };

  const renameGroup = async (path: string, newName: string) => {
    const seg = newName.trim().replace(/\//g, '');
    if (!seg) return;
    const parent = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
    const next = parent ? `${parent}/${seg}` : seg;
    if (next === path) return;
    try {
      await invoke('group_rename', { oldPath: path, newPath: next });
      persistExtras(rewritePathOnRename(extras, path, next));
      persistCollapsed(rewritePathOnRename([...collapsed], path, next));
      await s().loadSessions();
      s().notify(`分组已重命名为 ${next}`, 'success');
    } catch (e) {
      s().notify(`重命名失败: ${String(e)}`, 'error');
    }
  };

  const moveGroupTo = async (source: string, targetParent: string) => {
    const leaf = source.split('/').pop() ?? source;
    const next = targetParent ? `${targetParent}/${leaf}` : leaf;
    if (!canMoveGroup(source, next)) {
      s().notify('不能移动到它自己的子分组内', 'warning');
      return;
    }
    if (next === source) return;
    try {
      await invoke('group_rename', { oldPath: source, newPath: next });
      persistExtras(rewritePathOnRename(extras, source, next));
      persistCollapsed(rewritePathOnRename([...collapsed], source, next));
      await s().loadSessions();
      s().notify(`已移动到 ${next}`, 'success');
    } catch (e) {
      s().notify(`移动分组失败: ${String(e)}`, 'error');
    }
  };

  const deleteGroupKeep = async (path: string) => {
    try {
      const r = await invoke<{ affected: number }>('group_delete', {
        path,
        withSessions: false,
      });
      persistExtras(rewritePathOnDeleteKeep(extras, path));
      persistCollapsed(rewritePathOnDeleteKeep([...collapsed], path));
      await s().loadSessions();
      s().notify(`分组已删除，${r.affected} 台服务器已保留（直属移至未分组）`, 'success');
    } catch (e) {
      s().notify(`删除分组失败: ${String(e)}`, 'error');
    }
  };

  const deleteGroupWithSessions = async (path: string) => {
    try {
      const r = await invoke<{ affected: number }>('group_delete', {
        path,
        withSessions: true,
      });
      // 级联删除：该分组及子分组的 extras/collapsed 条目一并移除（不上移）
      const gone = (p: string) => p === path || p.startsWith(path + '/');
      persistExtras(extras.filter((p) => !gone(p)));
      persistCollapsed([...collapsed].filter((p) => !gone(p)));
      setSel({ anchor: null, ids: [] });
      await s().loadSessions();
      s().notify(`已删除分组及 ${r.affected} 台服务器`, 'success');
    } catch (e) {
      s().notify(`删除分组失败: ${String(e)}`, 'error');
    }
  };

  const moveSessions = async (ids: string[], target: string) => {
    const t = target.trim();
    if (!isValidGroupPath(t)) {
      s().notify(`分组路径无效: ${t}`, 'warning');
      return;
    }
    try {
      const r = await invoke<{ moved: number }>('session_move', {
        sessionIds: ids,
        groupPath: t,
      });
      await s().loadSessions();
      s().notify(`已移动 ${r.moved} 台服务器到 ${t || '未分组'}`, 'success');
    } catch (e) {
      s().notify(`移动失败: ${String(e)}`, 'error');
    }
  };

  const deleteSessions = async (recs: SessionRecord[]) => {
    let ok = 0;
    let fail = 0;
    for (const r of recs) {
      try {
        await invoke('session_delete', { sessionId: r.id });
        ok++;
      } catch {
        fail++;
      }
    }
    setSel({ anchor: null, ids: [] });
    await s().loadSessions();
    if (fail) s().notify(`删除完成：${ok} 成功，${fail} 失败`, 'warning');
    else s().notify(`已删除 ${ok} 台服务器`, 'success');
  };

  // ---------- 菜单 ----------

  const sessionMenu = (rec: SessionRecord): MenuItem[] => {
    const fav = favorites.has(rec.id);
    return [
      { label: '连接', onSelect: () => connect(rec) },
      { label: '在新标签连接', onSelect: () => connect(rec, 'new-tab') },
      { label: '在新窗口连接', onSelect: () => s().connectInNewWindow(rec.id, rec.name) },
      { label: '连接并打开 SFTP', onSelect: () => s().connectAndOpenSftp(rec.id, rec.name) },
      'separator',
      { label: '编辑', onSelect: () => openConnect(rec) },
      { label: '复制服务器', onSelect: () => void s().duplicateSession(rec) },
      {
        label: '移动到分组…',
        onSelect: () => setPrompt({ mode: 'move-to', ids: [rec.id], input: rec.groupPath }),
      },
      { label: fav ? '取消收藏' : '添加收藏', onSelect: () => s().toggleFavorite(rec.id) },
      'separator',
      {
        label: '复制主机地址',
        onSelect: () =>
          void writeText(`${rec.username}@${rec.host}:${rec.port}`).then(
            () => s().notify('已复制主机地址', 'success'),
            (e) => s().notify(`复制失败: ${e}`, 'error'),
          ),
      },
      {
        label: '复制 SSH 命令',
        onSelect: () =>
          void writeText(sshCommand(rec, sessions)).then(
            () => s().notify('已复制 SSH 命令', 'success'),
            (e) => s().notify(`复制失败: ${e}`, 'error'),
          ),
      },
      {
        label: '导出（复制 JSON）',
        onSelect: () =>
          void writeText(JSON.stringify(rec, null, 2)).then(
            () => s().notify('会话 JSON 已复制到剪贴板', 'success'),
            (e) => s().notify(`导出失败: ${e}`, 'error'),
          ),
      },
      'separator',
      { label: '删除', danger: true, onSelect: () => s().requestDeleteSession(rec) },
    ];
  };

  const batchMenu = (ids: string[]): MenuItem[] => [
    {
      label: `移动到分组…（${ids.length}）`,
      onSelect: () => setPrompt({ mode: 'move-to', ids, input: '' }),
    },
    {
      label: `添加收藏（${ids.length}）`,
      onSelect: () => {
        for (const id of ids) if (!favorites.has(id)) s().toggleFavorite(id);
      },
    },
    {
      label: `导出（复制 JSON，${ids.length}）`,
      onSelect: () => {
        const recs = sessions.filter((x) => ids.includes(x.id));
        void writeText(JSON.stringify(recs, null, 2)).then(
          () => s().notify(`已复制 ${recs.length} 台服务器 JSON`, 'success'),
          (e) => s().notify(`导出失败: ${e}`, 'error'),
        );
      },
    },
    'separator',
    {
      label: `删除（${ids.length}）…`,
      danger: true,
      onSelect: () => setBatchDel(sessions.filter((x) => ids.includes(x.id))),
    },
  ];

  const groupMenu = (path: string, count: number): MenuItem[] => [
    {
      label: '新建子分组…',
      onSelect: () => setPrompt({ mode: 'group-new', parent: path, input: '' }),
    },
    {
      label: '重命名…',
      onSelect: () =>
        setPrompt({ mode: 'group-rename', path, input: path.split('/').pop() ?? path }),
    },
    'separator',
    {
      label: count > 0 ? `删除分组（含 ${count} 台服务器）…` : '删除分组',
      danger: true,
      onSelect: () => {
        if (count > 0) setGroupDel({ path, count });
        else {
          persistExtras(extras.filter((p) => p !== path));
          persistCollapsed([...collapsed].filter((p) => p !== path));
          s().notify(`已删除空分组 ${path}`, 'success');
        }
      },
    },
  ];

  const blankMenu = (): MenuItem[] => [
    { label: '新建服务器', onSelect: () => openConnect() },
    {
      label: '新建根分组…',
      onSelect: () => setPrompt({ mode: 'group-new', parent: '', input: '' }),
    },
  ];

  // ---------- 拖拽 ----------

  const dragSessions = (e: React.DragEvent, rec: SessionRecord) => {
    const ids = sel.ids.includes(rec.id) && sel.ids.length > 1 ? sel.ids : [rec.id];
    e.dataTransfer.setData('application/x-myssh-sessions', JSON.stringify(ids));
  };

  const dropOnGroup = (e: React.DragEvent, path: string) => {
    e.preventDefault();
    // 阻止冒泡到根容器（根有自己的 drop → 移到未分组，会覆盖本目标）
    e.stopPropagation();
    setDropTarget(null);
    const ss = e.dataTransfer.getData('application/x-myssh-sessions');
    if (ss) {
      const ids = JSON.parse(ss) as string[];
      void moveSessions(ids, path);
      return;
    }
    const g = e.dataTransfer.getData('application/x-myssh-group');
    if (g) void moveGroupTo(g, path);
  };

  const dropOnRoot = (e: React.DragEvent) => {
    e.preventDefault();
    setDropTarget(null);
    const ss = e.dataTransfer.getData('application/x-myssh-sessions');
    if (ss) void moveSessions(JSON.parse(ss) as string[], '');
    else {
      const g = e.dataTransfer.getData('application/x-myssh-group');
      if (g) void moveGroupTo(g, '');
    }
  };

  // ---------- 渲染 ----------

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

  /** §10.4：键盘打开右键菜单（ContextMenu 键或 Shift+F10），位置取行矩形 */
  const menuKeyHit = (e: React.KeyboardEvent) =>
    e.key === 'ContextMenu' || (e.key === 'F10' && e.shiftKey);

  const sessionRow = (rec: SessionRecord, depth: number) => {
    const selected = sel.ids.includes(rec.id);
    return (
      <div
        key={rec.id}
        role="option"
        aria-selected={selected}
        tabIndex={0}
        draggable
        onDragStart={(e) => dragSessions(e, rec)}
        onDragOver={(e) => {
          // 拖到会话行 = 落入该行所在分组；阻止冒泡到根（未分组）处理器
          e.preventDefault();
          e.stopPropagation();
          setDropTarget(rec.groupPath);
        }}
        onDrop={(e) => dropOnGroup(e, rec.groupPath)}
        onClick={(e) => clickRow(rec, e)}
        onDoubleClick={() => connect(rec, 'new-tab')}
        onKeyDown={(e) => {
          if (e.key === 'Enter') connect(rec);
          else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            e.preventDefault();
            const i = visibleSessionIds.indexOf(rec.id);
            const next = visibleSessionIds[i + (e.key === 'ArrowDown' ? 1 : -1)];
            if (next) setSel({ anchor: next, ids: [next] });
          } else if (menuKeyHit(e)) {
            e.preventDefault();
            selectForMenu(rec);
            const ids = sel.ids.includes(rec.id) && sel.ids.length > 1 ? sel.ids : [rec.id];
            const r = e.currentTarget.getBoundingClientRect();
            setMenu({
              x: r.left + 16,
              y: r.bottom,
              items: ids.length > 1 ? batchMenu(ids) : sessionMenu(rec),
            });
          }
        }}
        onContextMenu={(e) => {
          e.preventDefault();
          selectForMenu(rec);
          const ids = sel.ids.includes(rec.id) && sel.ids.length > 1 ? sel.ids : [rec.id];
          setMenu({
            x: e.clientX,
            y: e.clientY,
            items: ids.length > 1 ? batchMenu(ids) : sessionMenu(rec),
          });
        }}
        className={`group flex cursor-pointer items-center rounded px-1 py-1 outline-none ${
          selected ? 'bg-neutral-700' : 'hover:bg-neutral-800 focus:bg-neutral-800'
        }`}
        style={{ paddingLeft: `${depth * 12 + 8}px` }}
        title={`${rec.username}@${rec.host}:${rec.port}`}
      >
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-1 text-sm text-neutral-200">
            {online.has(rec.id) && (
              <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-green-500" title="当前在线" />
            )}
            {favorites.has(rec.id) && <span className="shrink-0 text-amber-400">★</span>}
            <span className="truncate">{rec.name}</span>
            {failed.has(rec.id) && (
              <span className="shrink-0 text-red-400" title={`最近失败: ${failed.get(rec.id)}`}>
                ⚠
              </span>
            )}
          </span>
          <span className="block truncate text-xs text-neutral-500">
            {rec.username}@{rec.host}:{rec.port}
            {rec.jumpChain.length > 0 && ` · ${rec.jumpChain.length} 跳`}
          </span>
        </span>
        <span className="hidden shrink-0 gap-1 group-hover:flex group-focus-within:flex">
          <button
            className="rounded px-1 text-xs text-neutral-400 hover:text-green-400"
            title="连接"
            onClick={(e) => {
              e.stopPropagation();
              connect(rec);
            }}
          >
            ▶
          </button>
          <button
            className="rounded px-1 text-xs text-neutral-500 hover:text-neutral-200"
            onClick={(e) => {
              e.stopPropagation();
              openConnect(rec);
            }}
          >
            编辑
          </button>
          <button
            className="rounded px-1 text-xs text-neutral-500 hover:text-neutral-200"
            title="更多操作"
            onClick={(e) => {
              e.stopPropagation();
              selectForMenu(rec);
              const ids = sel.ids.includes(rec.id) && sel.ids.length > 1 ? sel.ids : [rec.id];
              const r = (e.target as HTMLElement).getBoundingClientRect();
              setMenu({
                x: r.right,
                y: r.top,
                items: ids.length > 1 ? batchMenu(ids) : sessionMenu(rec),
              });
            }}
          >
            ⋯
          </button>
        </span>
      </div>
    );
  };

  const groupRow = (path: string, depth: number, count: number) => {
    const name = path.split('/').pop() ?? path;
    const isCollapsed = collapsed.has(path);
    return (
      <div
        key={path}
        tabIndex={0}
        draggable
        onDragStart={(e) => e.dataTransfer.setData('application/x-myssh-group', path)}
        onDragOver={(e) => {
          e.preventDefault();
          e.stopPropagation(); // 同上：防根容器把悬停目标改写成未分组
          setDropTarget(path);
        }}
        onDragLeave={() => setDropTarget((t) => (t === path ? null : t))}
        onDrop={(e) => dropOnGroup(e, path)}
        onContextMenu={(e) => {
          e.preventDefault();
          setMenu({ x: e.clientX, y: e.clientY, items: groupMenu(path, count) });
        }}
        onKeyDown={(e) => {
          if (menuKeyHit(e)) {
            e.preventDefault();
            const r = e.currentTarget.getBoundingClientRect();
            setMenu({ x: r.left + 16, y: r.bottom, items: groupMenu(path, count) });
          }
        }}
        className={`group flex cursor-pointer select-none items-center rounded px-1 py-0.5 text-xs text-neutral-500 hover:bg-neutral-800 ${
          dropTarget === path ? 'ring-1 ring-blue-500' : ''
        }`}
        style={{ paddingLeft: `${depth * 12 + 4}px` }}
      >
        <button
          className="mr-0.5 w-4 shrink-0 text-center"
          aria-label={isCollapsed ? '展开分组' : '折叠分组'}
          onClick={() =>
            persistCollapsed(
              isCollapsed ? [...collapsed].filter((p) => p !== path) : [...collapsed, path],
            )
          }
        >
          {isCollapsed ? '▸' : '▾'}
        </button>
        <span className="min-w-0 flex-1 truncate" title={path}>
          {name}
        </span>
        <span className="shrink-0 text-neutral-600">{count}</span>
        <span className="hidden shrink-0 gap-0.5 group-hover:flex">
          <button
            className="rounded px-0.5 hover:text-neutral-200"
            title="新建子分组"
            onClick={() => setPrompt({ mode: 'group-new', parent: path, input: '' })}
          >
            ＋
          </button>
          <button
            className="rounded px-0.5 hover:text-neutral-200"
            title="重命名"
            onClick={() =>
              setPrompt({ mode: 'group-rename', path, input: path.split('/').pop() ?? path })
            }
          >
            ✏
          </button>
          <button
            className="rounded px-0.5 hover:text-red-400"
            title="删除分组"
            onClick={() => {
              if (count > 0) setGroupDel({ path, count });
              else {
                persistExtras(extras.filter((p) => p !== path));
                persistCollapsed([...collapsed].filter((p) => p !== path));
                s().notify(`已删除空分组 ${path}`, 'success');
              }
            }}
          >
            ×
          </button>
        </span>
      </div>
    );
  };

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
            {menuItem('新建根分组…', () => setPrompt({ mode: 'group-new', parent: '', input: '' }))}
            <div className="my-1 border-t border-neutral-800" />
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

      {/* 虚拟分组（8.5）：纯过滤视图，不改真实归属 */}
      <div className="flex flex-wrap gap-1 px-2 pb-1">
        {VIRTUAL_LABELS.map((v) => {
          const n =
            v.id === 'favorites'
              ? sessions.filter((x) => favorites.has(x.id)).length
              : v.id === 'recent'
                ? recent.filter((id) => sessions.some((x) => x.id === id)).length
                : v.id === 'online'
                  ? online.size
                  : v.id === 'ungrouped'
                    ? sessions.filter((x) => x.groupPath === '').length
                    : failed.size;
          return (
            <button
              key={v.id}
              className={`rounded px-1.5 py-0.5 text-xs ${
                virt === v.id
                  ? 'bg-blue-800 text-neutral-100'
                  : 'text-neutral-500 hover:bg-neutral-800'
              }`}
              title={
                v.id === 'failed' && failed.size > 0 ? [...failed.values()].join('\n') : v.label
              }
              onClick={() => setVirt((cur) => (cur === v.id ? null : v.id))}
            >
              {v.label}
              {n > 0 ? ` ${n}` : ''}
            </button>
          );
        })}
      </div>

      <div
        className={`flex-1 overflow-y-auto px-1 pb-2 ${dropTarget === '' ? 'ring-1 ring-inset ring-blue-500' : ''}`}
        onDragOver={(e) => {
          e.preventDefault();
          setDropTarget('');
        }}
        onDragLeave={() => setDropTarget((t) => (t === '' ? null : t))}
        onDrop={dropOnRoot}
        onContextMenu={(e) => {
          if (e.target === e.currentTarget) {
            e.preventDefault();
            setMenu({ x: e.clientX, y: e.clientY, items: blankMenu() });
          }
        }}
      >
        {sessions.length === 0 && (
          <p className="px-2 py-4 text-center text-xs text-neutral-600">
            还没有会话。点右上角 ＋ 新建，或 ⋯ 菜单导入。
          </p>
        )}
        {flatMode
          ? (virt ? virtList : filtered).map((rec) => sessionRow(rec, 0))
          : visibleRows.map((r) =>
              r.kind === 'session' && r.session
                ? sessionRow(r.session, r.depth)
                : groupRow(r.key, r.depth, r.group?.count ?? 0),
            )}
      </div>

      {/* 右键菜单 */}
      {menu && (
        <ContextMenu x={menu.x} y={menu.y} items={menu.items} onClose={() => setMenu(null)} />
      )}

      {/* 非空分组删除三选（统一 Dialog 基座：Esc=取消，默认焦点在安全选项） */}
      {groupDel && (
        <Dialog
          title="删除分组"
          onClose={() => setGroupDel(null)}
          closeOnBackdrop={false}
          panelClass="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
        >
          <h2 className="mb-2 text-base font-semibold text-neutral-100">
            删除分组“{groupDel.path}”？
          </h2>
          <p className="mb-4 text-xs leading-5 text-neutral-400">
            该分组及其子分组共包含 {groupDel.count} 台服务器。请选择处理方式：
          </p>
          <div className="flex flex-col gap-2">
            <button
              className="rounded border border-neutral-600 px-3 py-1.5 text-left text-xs hover:bg-neutral-800"
              data-autofocus
              onClick={() => {
                const p = groupDel.path;
                setGroupDel(null);
                void deleteGroupKeep(p);
              }}
            >
              仅删除分组，服务器移到未分组（子分组上移一级）
            </button>
            <button
              className="rounded border border-red-800 px-3 py-1.5 text-left text-xs text-red-300 hover:bg-red-950"
              onClick={() => {
                setConfirmCascade(groupDel); // 二次确认（危险）
                setGroupDel(null);
              }}
            >
              删除分组及其中 {groupDel.count} 台服务器（含已保存凭据）…
            </button>
            <button
              className="rounded px-3 py-1.5 text-xs text-neutral-400 hover:bg-neutral-800"
              onClick={() => setGroupDel(null)}
            >
              取消
            </button>
          </div>
        </Dialog>
      )}

      {/* 批量删除确认 */}
      {/* 级联删除的二次确认（凭据一并删除，不可撤销） */}
      {confirmCascade && (
        <ConfirmDialog
          title={`确认删除分组“${confirmCascade.path}”及全部服务器？`}
          confirmLabel={`永久删除 ${confirmCascade.count} 台服务器`}
          onCancel={() => setConfirmCascade(null)}
          onConfirm={() => {
            const p = confirmCascade.path;
            setConfirmCascade(null);
            void deleteGroupWithSessions(p);
          }}
        >
          <p className="mb-1">
            将删除分组 {confirmCascade.path} 下的 {confirmCascade.count} 台服务器，
            以及这些服务器保存的全部密码或凭据。
          </p>
          <p className="text-red-300">此操作无法撤销。</p>
        </ConfirmDialog>
      )}

      {batchDel && (
        <ConfirmDialog
          title={`删除 ${batchDel.length} 台服务器？`}
          confirmLabel={`删除 ${batchDel.length} 台服务器`}
          onCancel={() => setBatchDel(null)}
          onConfirm={() => {
            const recs = batchDel;
            setBatchDel(null);
            void deleteSessions(recs);
          }}
        >
          <p className="mb-1 max-h-32 overflow-y-auto break-all text-neutral-300">
            {batchDel.map((r) => r.name).join('、')}
          </p>
          <p className="mb-1">删除后，这些服务器保存的密码或凭据也会一并删除。</p>
          <p className="text-red-300">此操作无法撤销。</p>
        </ConfirmDialog>
      )}

      {/* 输入小弹层 */}
      {prompt && (
        <div className="absolute inset-0 z-30 flex items-center justify-center bg-black/60">
          <div className="w-72 rounded-lg border border-neutral-700 bg-neutral-900 p-3 text-xs text-neutral-200 shadow-xl">
            {prompt.mode === 'export-enc' && (
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
            )}
            {prompt.mode === 'import-cfg' && (
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
            {(prompt.mode === 'group-new' || prompt.mode === 'group-rename') && (
              <>
                <div className="mb-2">
                  {prompt.mode === 'group-new'
                    ? `新建${prompt.parent ? ` ${prompt.parent} 下的子分组` : '根分组'}`
                    : `重命名分组 ${prompt.path}`}
                </div>
                <input
                  className="mb-2 w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  placeholder="分组名（单层，不含 /）"
                  value={prompt.input}
                  onChange={(e) => setPrompt({ ...prompt, input: e.target.value })}
                  autoFocus
                />
              </>
            )}
            {prompt.mode === 'move-to' && (
              <>
                <div className="mb-2">移动 {prompt.ids.length} 台服务器到分组（留空 = 未分组）</div>
                <input
                  className="mb-2 w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                  placeholder="如：生产/Web"
                  list="myssh-group-paths"
                  value={prompt.input}
                  onChange={(e) => setPrompt({ ...prompt, input: e.target.value })}
                  autoFocus
                />
                <datalist id="myssh-group-paths">
                  {collectGroupPaths(tree).map((p) => (
                    <option key={p} value={p} />
                  ))}
                </datalist>
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
                  else if (p.mode === 'import-cfg')
                    void importConfigFile(p.input, p.pass || undefined);
                  else if (p.mode === 'group-new') createGroup(p.parent, p.input);
                  else if (p.mode === 'group-rename') void renameGroup(p.path, p.input);
                  else void moveSessions(p.ids, p.input);
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
