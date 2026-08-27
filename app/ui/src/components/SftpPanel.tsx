import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { listen } from '@tauri-apps/api/event';
import { useAppStore } from '../state/app-store';
import type { FileEntry } from '../term/types';
import { useTransferStore } from '../state/transfer-store';
import { ConfirmDialog } from './ConfirmDialog';
import { ContextMenu, type MenuItem } from './ContextMenu';
import { Dialog } from './Dialog';
import { PathBar } from './PathBar';
import {
  EMPTY_NAV_HIST,
  followTarget,
  navBack as histBack,
  navDropBack,
  navDropFwd,
  navFwd as histFwd,
  navPush,
  type NavHist,
} from './nav-hist';
import { usePanelHeight } from './panel-height';

/** 双栏 SFTP 面板：左本地 / 右远程，拖拽互传 + 终端 cwd 联动（OSC 7）。
 *  批次五：可编辑路径栏（前进/后退历史）、多选与批量操作、失败任务重试、
 *  本地文件操作、面板拖拽调高。远端操作命令见 crates/app/src/sftp.rs。
 *  批次六：拖拽增强（OS 拖入本地栏复制、拖到目录行进子目录、落点高亮）、
 *  复制路径、路径栏模糊建议、本地快捷位「桌面」、传输迁出至 TransferCenter、
 *  初始定位终端 cwd / 家目录（权限失败回退家目录）、跟随终端修复。
 *  批次十三：OS 拖放回归 Tauri 原生事件（dragDropEnabled=true）。批次七改走 HTML5
 *  是因为原生事件坐标为物理像素、高 DPI 下命中全偏；但 File 无完整路径，只能字节流
 *  经 local_drop_* 中转（大文件双份拷贝、无冲突确认）。现按 devicePixelRatio 换算回
 *  CSS 像素命中，拿到真实路径后与面板「上传 →」共用 uploadPathsTo（冲突确认/目录
 *  递归/续传策略一致）；拖入本地栏走 local_copy。
 *  限制：远程 → 窗外 OS 拖出下载在 Tauri webview 无原生支持（不提供 DragOut/
 *  拖拽数据供给），且不允许新增依赖，故不实现。 */

type Side = 'local' | 'remote';

function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} K`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1048576).toFixed(1)} M`;
  return `${(n / 1073741824).toFixed(2)} G`;
}

function fmtTime(unix?: number | null): string {
  if (!unix) return '';
  const d = new Date(unix * 1000);
  return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

function fmtPerms(p?: number | null): string {
  if (p == null) return '';
  return (p & 0o7777).toString(8).padStart(4, '0');
}

/** 权限位 → 中文三段（拥有者/同组/其他），如 0755 → 读写执/读执/读执；无权限段显示「无」 */
function fmtPermsCn(p?: number | null): string {
  if (p == null) return '';
  const seg = (b: number) => `${b & 4 ? '读' : ''}${b & 2 ? '写' : ''}${b & 1 ? '执' : ''}` || '无';
  return `${seg((p >> 6) & 7)}/${seg((p >> 3) & 7)}/${seg(p & 7)}`;
}

/** 权限列 tooltip：三段完整描述 + 原始八进制 */
function permsTitle(p?: number | null): string {
  if (p == null) return '';
  const seg = (b: number) =>
    [b & 4 ? '读' : '', b & 2 ? '写' : '', b & 1 ? '执行' : ''].filter(Boolean).join('、') ||
    '无权限';
  return `拥有者：${seg((p >> 6) & 7)}；同组：${seg((p >> 3) & 7)}；其他：${seg(p & 7)}（八进制 ${fmtPerms(p)}）`;
}

/** 目录/名拼接（base='/' 或 '' 不产生双斜杠；本地 Windows 路径同样适用） */
function joinRemote(base: string, rel: string): string {
  return base === '/' || base === '' ? `/${rel}` : `${base}/${rel}`;
}

/** 末段文件名（本地 \ 与远程 / 通用） */
function baseNameOf(p: string): string {
  const norm = p.replace(/\\/g, '/').replace(/\/+$/, '');
  return norm.split('/').pop() || norm;
}

/** 目标已存在时的处理策略（sftp_upload/sftp_download 的 onExists 参数） */
type OnExists = 'resume' | 'overwrite' | 'skip' | 'rename';

function parentDir(path: string, remote: boolean): string {
  if (remote) {
    if (path === '/' || path === '') return '/';
    const trimmed = path.replace(/\/+$/, '');
    const idx = trimmed.lastIndexOf('/');
    return idx <= 0 ? '/' : trimmed.slice(0, idx);
  }
  const norm = path.replace(/\\/g, '/').replace(/\/+$/, '');
  if (/^[A-Z]:\/?$/i.test(norm)) return ''; // 盘符根 → 盘符枚举
  const idx = norm.lastIndexOf('/');
  if (idx < 0) return '';
  if (idx === 2 && norm[1] === ':') return norm.slice(0, 3);
  return norm.slice(0, idx);
}

interface PaneProps {
  side: Side;
  entries: FileEntry[];
  path: string;
  loading: boolean;
  canBack: boolean;
  canFwd: boolean;
  sel: Set<string>;
  onRowClick: (e: FileEntry, ev: React.MouseEvent) => void;
  onOpen: (e: FileEntry) => void;
  onNavigate: (path: string) => Promise<boolean>;
  onBack: () => void;
  onFwd: () => void;
  onUp: () => void;
  onRefresh: () => void;
  onDropEntries: (paths: string[]) => void;
  /** 落到目录行上：传入该子目录路径（批次六 1b） */
  onDropEntriesInto: (paths: string[], dir: string) => void;
  onRowMenu: (e: FileEntry, x: number, y: number) => void;
  onClearSel: () => void;
  onSelectAll: () => void;
  /** 方向键移动选中（批次十一 3；pane 按可见序算好目标行，父级只管选中） */
  onArrowSelect: (e: FileEntry) => void;
  /** Delete/F2 委托父级（删除确认/重命名输入态在父级）；Enter/Backspace/方向键 pane 内处理 */
  onRowKey: (e: FileEntry, ev: React.KeyboardEvent) => void;
  /** 路径栏模糊建议（批次六 3） */
  fetchSuggestions: (input: string) => Promise<string[]>;
  /** 落点高亮：内部拖拽悬停 或 OS 文件悬停（批次六 1d） */
  dropActive: boolean;
  onDropHover: (active: boolean) => void;
  /** 快捷位置条（批次六 4，本地栏：此电脑/桌面）；远程栏传 null */
  quickSlots?: React.ReactNode;
}

function FilePane(p: PaneProps) {
  // 行级落点（批次六 1b）：悬停的目录行路径；null=落当前目录
  const [rowDrop, setRowDrop] = useState<string | null>(null);
  // 列排序（批次十一 4）：目录恒排文件前；点击同列切换升降序，点击异列升序
  const [sortKey, setSortKey] = useState<'name' | 'size' | 'mtime'>('name');
  const [sortAsc, setSortAsc] = useState(true);
  // 即时过滤 + 隐藏文件开关（批次十一 5；会话内存态，不持久化）
  const [filter, setFilter] = useState('');
  const [showHidden, setShowHidden] = useState(false);

  const visible = useMemo(() => {
    let list = p.entries;
    if (!showHidden) list = list.filter((e) => !e.name.startsWith('.'));
    const f = filter.trim().toLowerCase();
    if (f) list = list.filter((e) => e.name.toLowerCase().includes(f));
    return [...list].sort((a, b) => {
      const da = a.kind === 'dir' ? 0 : 1;
      const db = b.kind === 'dir' ? 0 : 1;
      if (da !== db) return da - db;
      const c =
        sortKey === 'name'
          ? a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' })
          : sortKey === 'size'
            ? a.size - b.size
            : (a.mtime ?? 0) - (b.mtime ?? 0);
      return sortAsc ? c : -c;
    });
  }, [p.entries, filter, showHidden, sortKey, sortAsc]);

  const sortBy = (key: 'name' | 'size' | 'mtime') => {
    if (sortKey === key) setSortAsc((v) => !v);
    else {
      setSortKey(key);
      setSortAsc(true);
    }
  };
  const sortMark = (key: 'name' | 'size' | 'mtime') =>
    sortKey === key ? (sortAsc ? ' ▲' : ' ▼') : '';
  const hbtn = 'cursor-pointer select-none hover:text-neutral-300';
  return (
    <div
      className={`flex min-h-0 flex-1 flex-col ${p.dropActive ? 'ring-2 ring-inset ring-blue-600' : ''}`}
      data-drop-side={p.side}
      onDragOver={(e) => {
        e.preventDefault();
        p.onDropHover(true);
      }}
      onDragLeave={(e) => {
        if (e.currentTarget.contains(e.relatedTarget as Node)) return;
        p.onDropHover(false);
        setRowDrop(null);
      }}
      onDrop={(e) => {
        e.preventDefault();
        p.onDropHover(false);
        const raw = e.dataTransfer.getData('application/x-myssh-entry');
        const dir = rowDrop;
        setRowDrop(null);
        if (raw) {
          const src = JSON.parse(raw) as { side: string; paths: string[] };
          if (src.side !== p.side) {
            if (dir) p.onDropEntriesInto(src.paths, dir);
            else p.onDropEntries(src.paths);
          }
          return;
        }
        // OS 文件拖入走 Tauri 原生事件（dragDropEnabled=true），不会进 HTML5 drop
      }}
    >
      <PathBar
        path={p.path}
        placeholder={p.side === 'local' ? '此电脑' : '/'}
        loading={p.loading}
        canBack={p.canBack}
        canFwd={p.canFwd}
        onBack={p.onBack}
        onFwd={p.onFwd}
        onUp={p.onUp}
        onRefresh={p.onRefresh}
        onNavigate={p.onNavigate}
        fetchSuggestions={p.fetchSuggestions}
      />
      {p.quickSlots}
      {/* 过滤 + 隐藏文件开关（批次十一 5） */}
      <div className="flex items-center gap-2 border-b border-neutral-800 px-2 py-0.5 text-neutral-500">
        <input
          className="min-w-0 flex-1 rounded border border-neutral-700 bg-neutral-800 px-1.5 py-0.5 text-neutral-200 outline-none placeholder:text-neutral-600 focus:border-blue-600"
          placeholder="过滤…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <label className="flex shrink-0 cursor-pointer items-center gap-1">
          <input
            type="checkbox"
            checked={showHidden}
            onChange={(e) => setShowHidden(e.target.checked)}
          />
          显示隐藏
        </label>
      </div>
      {/* 列头（批次十一 4：名称/大小/修改时间可点排序，目录恒排文件前） */}
      <div className="flex items-center gap-2 border-b border-neutral-800 px-2 py-0.5 text-neutral-500">
        <span className="w-4 shrink-0" />
        <button className={`${hbtn} min-w-0 flex-1 text-left`} onClick={() => sortBy('name')}>
          名称{sortMark('name')}
        </button>
        <button className={`${hbtn} w-16 shrink-0 text-right`} onClick={() => sortBy('size')}>
          大小{sortMark('size')}
        </button>
        <button className={`${hbtn} w-16 shrink-0 text-right`} onClick={() => sortBy('mtime')}>
          修改时间{sortMark('mtime')}
        </button>
        {p.side === 'remote' && <span className="w-32 shrink-0 text-right">权限</span>}
      </div>
      <div
        className="min-h-0 flex-1 overflow-y-auto outline-none"
        tabIndex={0}
        onClick={(e) => {
          if (e.target === e.currentTarget) p.onClearSel();
        }}
        onKeyDown={(e) => {
          if ((e.ctrlKey || e.metaKey) && (e.key === 'a' || e.key === 'A')) {
            e.preventDefault();
            p.onSelectAll();
          }
        }}
      >
        {visible.map((e) => (
          <div
            key={e.path}
            role="option"
            aria-selected={p.sel.has(e.path)}
            data-dir-row={e.kind === 'dir' ? e.path : undefined}
            tabIndex={0}
            draggable
            onDragStart={(ev) =>
              ev.dataTransfer.setData(
                'application/x-myssh-entry',
                JSON.stringify({
                  side: p.side,
                  paths: p.sel.has(e.path) ? [...p.sel] : [e.path],
                }),
              )
            }
            onDragOver={
              e.kind === 'dir'
                ? (ev) => {
                    // 目录行可接收对面栏条目（落到该子目录）；OS 文件走原生事件，不进这里
                    if (ev.dataTransfer.types.includes('application/x-myssh-entry')) {
                      ev.preventDefault();
                      ev.stopPropagation();
                      setRowDrop(e.path);
                    }
                  }
                : undefined
            }
            onDragLeave={
              e.kind === 'dir' ? () => setRowDrop((d) => (d === e.path ? null : d)) : undefined
            }
            onDrop={
              e.kind === 'dir'
                ? (ev) => {
                    const raw = ev.dataTransfer.getData('application/x-myssh-entry');
                    // 非内部负载（OS 文件）不拦截：冒泡给栏位级 onDrop 统一处理
                    if (!raw) return;
                    ev.preventDefault();
                    ev.stopPropagation();
                    setRowDrop(null);
                    p.onDropHover(false);
                    const src = JSON.parse(raw) as { side: string; paths: string[] };
                    if (src.side !== p.side) p.onDropEntriesInto(src.paths, e.path);
                  }
                : undefined
            }
            onClick={(ev) => p.onRowClick(e, ev)}
            onDoubleClick={() => p.onOpen(e)}
            onContextMenu={(ev) => {
              ev.preventDefault();
              p.onRowMenu(e, ev.clientX, ev.clientY);
            }}
            onKeyDown={(ev) => {
              // 键盘导航（批次十一 3，风格对齐 Sidebar）：↑↓ 移选中且焦点跟随，
              // Enter 打开/进目录，Backspace 回上级，Delete/F2 交父级确认/输入
              if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
                ev.preventDefault();
                const i = visible.findIndex((x) => x.path === e.path);
                const next = visible[i + (ev.key === 'ArrowDown' ? 1 : -1)];
                if (next) {
                  p.onArrowSelect(next);
                  const sib = (
                    ev.key === 'ArrowDown'
                      ? ev.currentTarget.nextElementSibling
                      : ev.currentTarget.previousElementSibling
                  ) as HTMLElement | null;
                  sib?.focus();
                }
                return;
              }
              if (ev.key === 'Enter') {
                ev.preventDefault();
                p.onOpen(e);
                return;
              }
              if (ev.key === 'Backspace') {
                ev.preventDefault();
                p.onUp();
                return;
              }
              p.onRowKey(e, ev);
            }}
            className={`flex cursor-pointer items-center gap-2 px-2 py-0.5 text-xs ${
              rowDrop === e.path
                ? 'bg-blue-900/60 text-neutral-100 outline outline-1 outline-blue-600'
                : p.sel.has(e.path)
                  ? 'bg-neutral-700 text-neutral-100 outline-none'
                  : 'text-neutral-300 outline-none hover:bg-neutral-800'
            }`}
          >
            <span className="w-4 shrink-0 text-center">
              {e.kind === 'dir' ? '📁' : e.kind === 'symlink' ? '🔗' : '📄'}
            </span>
            <span className="min-w-0 flex-1 truncate" title={e.name}>
              {e.name}
            </span>
            <span className="w-16 shrink-0 text-right text-neutral-500">
              {e.kind === 'dir' ? '' : fmtSize(e.size)}
            </span>
            <span className="w-16 shrink-0 text-right text-neutral-600">{fmtTime(e.mtime)}</span>
            {p.side === 'remote' && (
              <span
                className="w-32 shrink-0 text-right text-neutral-600"
                title={permsTitle(e.permissions)}
              >
                {fmtPermsCn(e.permissions)}
              </span>
            )}
          </div>
        ))}
        {!p.loading && visible.length === 0 && (
          <div className="px-3 py-4 text-xs text-neutral-600">
            {p.entries.length === 0 ? '（空目录）' : '（无匹配项）'}
          </div>
        )}
      </div>
    </div>
  );
}

export function SftpPanel({ tabId }: { tabId: string }) {
  const tabs = useAppStore((s) => s.tabs);
  const toggleSftp = useAppStore((s) => s.toggleSftp);
  const notify = useAppStore((s) => s.notify);
  const sessions = useAppStore((s) => s.sessions);
  const tab = tabs.find((t) => t.id === tabId);
  const rawSessionId = tab?.target.kind === 'session' ? tab.target.sessionId : null;
  // 本地会话：SFTP 无意义（本机文件即左栏），按无会话处理走守卫页
  const isLocalSession = sessions.find((r) => r.id === rawSessionId)?.kind === 'local';
  const sessionId = isLocalSession ? null : rawSessionId;

  const [localPath, setLocalPath] = useState('');
  const [remotePath, setRemotePath] = useState('/');
  const [localHist, setLocalHist] = useState<NavHist>(EMPTY_NAV_HIST);
  const [remoteHist, setRemoteHist] = useState<NavHist>(EMPTY_NAV_HIST);
  const [localEntries, setLocalEntries] = useState<FileEntry[]>([]);
  const [remoteEntries, setRemoteEntries] = useState<FileEntry[]>([]);
  const [localLoading, setLocalLoading] = useState(false);
  const [remoteLoading, setRemoteLoading] = useState(false);
  /** 多选（11.2）：paths 为绝对路径集合；anchor 是 Shift 连选基准 */
  const [sel, setSel] = useState<{ side: Side; paths: Set<string>; anchor: string | null }>({
    side: 'local',
    paths: new Set(),
    anchor: null,
  });
  const [followTerm, setFollowTerm] = useState(true);
  const [prompt, setPrompt] = useState<{
    action: 'mkdir' | 'rename' | 'chmod' | 'move' | 'touch';
    side: Side;
    value: string;
  } | null>(null);
  /** 待确认删除的条目（11.2 批量；目录递归删除，无法恢复） */
  const [confirmDel, setConfirmDel] = useState<FileEntry[] | null>(null);
  /** 覆盖确认（批次十一 1）：整批冲突统一策略；resolve(null)=取消入队 */
  const [conflictAsk, setConflictAsk] = useState<{
    count: number;
    resolve: (policy: OnExists | null) => void;
  } | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; side: Side } | null>(null);
  /** 常用路径 chip 右键菜单目标（批次十 3 本地；批次十一 6 远程） */
  const [favMenu, setFavMenu] = useState<{ x: number; y: number; path: string; side: Side } | null>(
    null,
  );
  const appSettings = useAppStore((s) => s.settings);
  const setSetting = useAppStore((s) => s.setSetting);
  /** 本会话传输快照（transfer-store 聚合；订阅由初始 effect 幂等建立） */
  const sessionTransfers = useTransferStore((s) =>
    sessionId ? s.bySession[sessionId] : undefined,
  );
  /** 拖拽悬停的栏位（落点高亮） */
  const [html5DragSide, setHtml5DragSide] = useState<Side | null>(null);
  const { height, handle } = usePanelHeight('sftp.height', 256);
  // 最新路径/刷新函数的 ref：1s 跟随轮询与 OS 拖放回调读取，避免闭包过期（批次六 10）
  const remotePathRef = useRef(remotePath);
  const localPathRef = useRef(localPath);
  const refreshRemoteRef = useRef<(path?: string) => Promise<string | null>>(async () => null);
  const refreshLocalRef = useRef<(path?: string) => Promise<string | null>>(async () => null);
  /** 已解析的远端家目录（sftp_home 缓存；解析失败退化为 "."） */
  const homeRef = useRef<string | null>(null);
  /** 上次跟随导航到的 cwd（去重，见 followTarget） */
  const lastFollowRef = useRef<string | null>(null);

  const clearSel = useCallback(() => setSel((s) => ({ ...s, paths: new Set(), anchor: null })), []);

  const refreshLocal = useCallback(
    async (path?: string): Promise<string | null> => {
      const target = path ?? localPath;
      setLocalLoading(true);
      try {
        const res = await invoke<{ entries: FileEntry[]; path: string }>('local_list', {
          path: target,
        });
        setLocalEntries(res.entries);
        setLocalPath(res.path);
        return res.path;
      } catch (e) {
        notify(`本地目录读取失败: ${e}`, 'error');
        return null;
      } finally {
        setLocalLoading(false);
      }
    },
    [localPath, notify],
  );

  /** 解析远端家目录（批次六 9）：sftp_home 一次缓存；服务器无 expand-path 扩展时
   *  退化为 "."（SFTP 会话默认起点即家目录，不落 "/"） */
  const resolveHome = useCallback(async (): Promise<string> => {
    if (homeRef.current) return homeRef.current;
    try {
      const home = await invoke<string>('sftp_home', { sessionId });
      homeRef.current = home;
      return home;
    } catch {
      homeRef.current = '.';
      return '.';
    }
  }, [sessionId]);

  const refreshRemote = useCallback(
    async (path?: string, allowFallback = true): Promise<string | null> => {
      if (!sessionId) return null;
      // 批次六 9：权限等失败回退家目录（warning），不停在原地报错；回退只尝试一次（循环而非递归，
      // 避免 useCallback 自引用触发 react-hooks/immutability）
      let target = path ?? remotePath;
      for (;;) {
        setRemoteLoading(true);
        try {
          const res = await invoke<{ entries: FileEntry[] }>('sftp_list', {
            sessionId,
            path: target,
          });
          setRemoteEntries(res.entries);
          setRemotePath(target);
          return target;
        } catch (e) {
          if (allowFallback) {
            const home = await resolveHome();
            if (home !== target) {
              notify(`无法读取 ${target}（${e}），已回退到家目录`, 'warning');
              target = home;
              allowFallback = false;
              continue;
            }
          }
          notify(`远程目录读取失败: ${e}`, 'error');
          return null;
        } finally {
          setRemoteLoading(false);
        }
      }
    },
    [sessionId, remotePath, notify, resolveHome],
  );

  // 跟随轮询 / OS 拖放回调经 ref 读最新值（闭包过期修复，批次六 10）
  useEffect(() => {
    remotePathRef.current = remotePath;
    localPathRef.current = localPath;
    refreshRemoteRef.current = refreshRemote;
    refreshLocalRef.current = refreshLocal;
  }, [remotePath, localPath, refreshRemote, refreshLocal]);

  /** 面板根节点：原生拖放命中测试限定本面板 DOM（多窗口/隐藏页签不误触发） */
  const rootRef = useRef<HTMLDivElement>(null);
  /** 原生 OS 拖放处理器：无依赖 effect 每渲染同步最新闭包，监听器内经 ref 调用 */
  const osDropRef = useRef<(paths: string[], side: Side, dir: string | null) => void>(() => {
    /* 首帧占位，随即被下方 effect 同步为真实闭包 */
  });
  useEffect(() => {
    osDropRef.current = (paths, side, dir) => {
      if (paths.length === 0) return;
      if (side === 'remote') {
        if (!sessionId) return;
        // 与面板「上传 →」完全同路径：冲突确认/目录递归/续传策略一致
        uploadPathsTo(paths, dir ?? remotePath);
        return;
      }
      void (async () => {
        const target = dir ?? localPath;
        if (!target) {
          notify('盘符枚举页无法接收文件，请先进入一个本地目录', 'warning');
          return;
        }
        let ok = 0;
        for (const src of paths) {
          try {
            await invoke('local_copy', { from: src, toDir: target });
            ok++;
          } catch (e) {
            notify(`复制失败: ${e}`, 'error');
          }
        }
        if (ok > 0) {
          notify(`已复制 ${ok} 个项目`, 'success');
          void refreshLocal();
        }
      })();
    };
  });

  // 批次十三：Tauri 原生拖放监听。position 是物理像素（DragDropEvent 为
  // dpi::PhysicalPosition），高 DPI 下必须除以 devicePixelRatio 换回 CSS 像素再做
  // elementFromPoint 命中——批次七正是漏了这步导致 150% 缩放全偏。drag-over 连续给
  // 位置，兼做落点栏高亮；命中目录行（data-dir-row）则落进该子目录。
  useEffect(() => {
    interface DragPayload {
      paths: string[];
      position: { x: number; y: number };
    }
    const hit = (pos: { x: number; y: number }): { side: Side; dir: string | null } | null => {
      const dpr = window.devicePixelRatio || 1;
      const el = document.elementFromPoint(pos.x / dpr, pos.y / dpr);
      if (!el || !rootRef.current?.contains(el)) return null;
      const pane = el.closest('[data-drop-side]');
      if (!pane) return null;
      const side: Side = pane.getAttribute('data-drop-side') === 'remote' ? 'remote' : 'local';
      const dir = el.closest('[data-dir-row]')?.getAttribute('data-dir-row') ?? null;
      return { side, dir };
    };
    let lastHover: Side | null = null;
    const offs: (() => void)[] = [];
    void listen<DragPayload>('tauri://drag-over', (ev) => {
      const side = hit(ev.payload.position)?.side ?? null;
      if (side !== lastHover) {
        lastHover = side;
        setHtml5DragSide(side);
      }
    }).then((off) => offs.push(off));
    void listen<DragPayload>('tauri://drag-drop', (ev) => {
      lastHover = null;
      setHtml5DragSide(null);
      const h = hit(ev.payload.position);
      if (h) osDropRef.current(ev.payload.paths, h.side, h.dir);
    }).then((off) => offs.push(off));
    void listen('tauri://drag-leave', () => {
      lastHover = null;
      setHtml5DragSide(null);
    }).then((off) => offs.push(off));
    return () => offs.forEach((off) => off());
  }, []);

  // 初始加载 + 传输订阅（微任务推迟 setState，避开 effect 内同步渲染级联）
  useEffect(() => {
    if (!sessionId) return;
    queueMicrotask(() => {
      // 批次六 9：优先定位终端 cwd；shell 未上报（null）时解析家目录，不落 "/"
      const t = useAppStore.getState().tabs.find((x) => x.id === tabId);
      const p = t ? t.panes[t.activePaneId] : null;
      const cwd = p?.session.cwd ?? null;
      void (async () => {
        await refreshRemoteRef.current(cwd ?? (await resolveHome()));
      })();
      void refreshLocalRef.current('');
    });
    // 传输订阅迁至 transfer-store（幂等；状态栏活跃数由 store 聚合发布）
    useTransferStore.getState().ensureSession(sessionId);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  // 终端 cwd 跟随（OSC 7；非用户导航 → 不入历史栈）。
  // 批次六 10 修复：不再捕获渲染期的 pane/remotePath（pane 对象身份不随 cwd 变化，
  // tab 切换后引用过期），改为 tick 内从 store 按 tabId 取最新 pane，followTarget 去重。
  useEffect(() => {
    if (!followTerm || !sessionId) return;
    const timer = setInterval(() => {
      const t = useAppStore.getState().tabs.find((x) => x.id === tabId);
      const p = t ? t.panes[t.activePaneId] : null;
      const target = followTarget(
        lastFollowRef.current,
        p?.session.cwd ?? null,
        remotePathRef.current,
      );
      if (target) {
        lastFollowRef.current = target;
        void refreshRemoteRef.current(target);
      }
    }, 1000);
    return () => clearInterval(timer);
  }, [followTerm, sessionId, tabId]);

  // 批次十二：传输达终态自动刷新目标栏（上传→远程栏，下载→本地栏；失败/取消可能留有
  // 残件，一并刷新）。此前上传完列表不动，用户看不到新文件。500ms 防抖合并批量完成；
  // seenTerminal 去重——队列保留终态项，后续每帧都会再带一次。
  const seenTerminalRef = useRef<Set<string>>(new Set());
  const refreshTimerRef = useRef<{ local?: number; remote?: number }>({});
  useEffect(() => {
    const transfers = sessionTransfers ?? [];
    let remote = false;
    let local = false;
    for (const t of transfers) {
      if (t.history || (t.state !== 'done' && t.state !== 'failed' && t.state !== 'canceled'))
        continue;
      if (seenTerminalRef.current.has(t.id)) continue;
      seenTerminalRef.current.add(t.id);
      if (t.direction === 'upload') remote = true;
      else local = true;
    }
    if (seenTerminalRef.current.size > 1000) seenTerminalRef.current.clear();
    const timers = refreshTimerRef.current;
    const schedule = (side: 'local' | 'remote') => {
      if (timers[side]) window.clearTimeout(timers[side]);
      timers[side] = window.setTimeout(() => {
        if (side === 'remote') void refreshRemoteRef.current();
        else void refreshLocalRef.current();
      }, 500);
    };
    if (remote) schedule('remote');
    if (local) schedule('local');
  }, [sessionTransfers]);
  useEffect(() => {
    const timers = refreshTimerRef.current;
    return () => {
      if (timers.local) window.clearTimeout(timers.local);
      if (timers.remote) window.clearTimeout(timers.remote);
    };
  }, []);

  // 终端右键「打开 SFTP」且面板已开：消费导航请求定位到 cwd（批次六 9；不入历史栈）
  const navReq = useTransferStore((s) => s.navRequests[tabId]);
  useEffect(() => {
    if (!navReq || !sessionId) return;
    useTransferStore.getState().consumeNav(tabId);
    void refreshRemoteRef.current(navReq);
  }, [navReq, sessionId, tabId]);

  if (!sessionId) {
    return (
      <div className="border-t border-neutral-800 bg-neutral-900 px-3 py-2 text-xs text-neutral-500">
        {isLocalSession
          ? '本地会话无需 SFTP——文件就在本机，终端里直接操作即可'
          : 'SFTP 仅支持存储档案会话（内联连接无档案可解析凭据）'}
        <button className="ml-2 text-neutral-400" onClick={() => toggleSftp(tabId)}>
          关闭
        </button>
      </div>
    );
  }

  // ---------- 导航（用户导航才入历史栈） ----------

  const navSide = async (side: Side, target: string, push = true): Promise<boolean> => {
    const prev = side === 'local' ? localPath : remotePath;
    const resolved = side === 'local' ? await refreshLocal(target) : await refreshRemote(target);
    if (resolved === null) return false;
    if (push) {
      const setHist = side === 'local' ? setLocalHist : setRemoteHist;
      setHist((h) => navPush(h, prev, resolved));
    }
    clearSel();
    return true;
  };

  const navBack = async (side: Side) => {
    const hist = side === 'local' ? localHist : remoteHist;
    const setHist = side === 'local' ? setLocalHist : setRemoteHist;
    const prev = side === 'local' ? localPath : remotePath;
    const step = histBack(hist, prev);
    if (!step) return;
    const target = step.target;
    const resolved = side === 'local' ? await refreshLocal(target) : await refreshRemote(target);
    if (resolved === null) {
      setHist(navDropBack); // 目标已失效：出栈
      return;
    }
    setHist(step.hist);
    clearSel();
  };

  const navFwd = async (side: Side) => {
    const hist = side === 'local' ? localHist : remoteHist;
    const setHist = side === 'local' ? setLocalHist : setRemoteHist;
    const prev = side === 'local' ? localPath : remotePath;
    const step = histFwd(hist, prev);
    if (!step) return;
    const target = step.target;
    const resolved = side === 'local' ? await refreshLocal(target) : await refreshRemote(target);
    if (resolved === null) {
      setHist(navDropFwd);
      return;
    }
    setHist(step.hist);
    clearSel();
  };

  // ---------- 选择 ----------

  const selEntries = (side: Side): FileEntry[] => {
    if (sel.side !== side) return [];
    return (side === 'local' ? localEntries : remoteEntries).filter((e) => sel.paths.has(e.path));
  };

  const rowClick = (side: Side) => (entry: FileEntry, ev: React.MouseEvent) => {
    const entries = side === 'local' ? localEntries : remoteEntries;
    if (ev.ctrlKey || ev.metaKey) {
      setSel((s) => {
        const paths = s.side === side ? new Set(s.paths) : new Set<string>();
        if (paths.has(entry.path)) paths.delete(entry.path);
        else paths.add(entry.path);
        return { side, paths, anchor: entry.path };
      });
    } else if (ev.shiftKey && sel.side === side && sel.anchor) {
      const a = entries.findIndex((e) => e.path === sel.anchor);
      const b = entries.findIndex((e) => e.path === entry.path);
      if (a >= 0 && b >= 0) {
        const [lo, hi] = a < b ? [a, b] : [b, a];
        setSel({
          side,
          paths: new Set(entries.slice(lo, hi + 1).map((e) => e.path)),
          anchor: sel.anchor,
        });
      }
    } else {
      setSel({ side, paths: new Set([entry.path]), anchor: entry.path });
    }
  };

  const openEntry = (side: Side) => (e: FileEntry) => {
    if (e.kind === 'dir') {
      void navSide(side, e.path);
    } else if (side === 'local') {
      void invoke('open_in_explorer', { path: e.path }).catch((err) =>
        notify(`打开失败: ${err}`, 'error'),
      );
    } else if (e.kind === 'file') {
      // 双击远程文件 = 直编（下载临时区 + 监听回传 + 系统默认编辑器）
      void (async () => {
        try {
          const res = await invoke<{ localPath: string }>('sftp_edit_open', {
            sessionId,
            remote: e.path,
          });
          await invoke('open_local', { path: res.localPath });
          notify(`已打开编辑: ${e.name}（保存即回传）`, 'success');
        } catch (err) {
          notify(`编辑打开失败: ${err}`, 'error');
        }
      })();
    }
  };

  /** 方向键移动选中（批次十一 3）：pane 已按可见序算好目标行，这里只管单选 */
  const arrowSelect = (side: Side) => (entry: FileEntry) =>
    setSel({ side, paths: new Set([entry.path]), anchor: entry.path });

  /** Delete/F2（批次十一 3）：Delete 走现有批量确认（选中集含焦点行则整集删除），F2 开重命名 */
  const rowKey = (side: Side) => (entry: FileEntry, ev: React.KeyboardEvent) => {
    if (ev.key === 'Delete') {
      ev.preventDefault();
      const inSel = sel.side === side && sel.paths.has(entry.path);
      const targets = inSel ? selEntries(side) : [entry];
      if (!inSel) setSel({ side, paths: new Set([entry.path]), anchor: entry.path });
      if (targets.length > 0) setConfirmDel(targets);
    } else if (ev.key === 'F2') {
      ev.preventDefault();
      setPrompt({ action: 'rename', side, value: entry.name });
    }
  };

  // ---------- 传输 ----------

  /** 覆盖确认（批次十一 1）：入队前逐个 stat 目标（sftp_stat 报错=不存在；local_stat 读 exists）。
   *  无冲突返回 'none'（不弹窗、不传 onExists，保持默认续传）；有冲突整批弹一次策略对话框，
   *  用户取消返回 null（整批不入队）。 */
  const pickOnExists = async (
    targets: string[],
    statOne: (t: string) => Promise<boolean>,
  ): Promise<OnExists | 'none' | null> => {
    let conflicts = 0;
    for (const t of targets) {
      try {
        if (await statOne(t)) conflicts++;
      } catch {
        // 目标不存在（sftp_stat 报错）→ 非冲突
      }
    }
    if (conflicts === 0) return 'none';
    const { promise, resolve } = Promise.withResolvers<OnExists | null>();
    setConflictAsk({ count: conflicts, resolve });
    return promise;
  };

  const resolveConflict = (policy: OnExists | null) => {
    conflictAsk?.resolve(policy);
    setConflictAsk(null);
  };

  const uploadPathsTo = (paths: string[], dir: string) => {
    void (async () => {
      const policy = await pickOnExists(
        paths.map((p) => joinRemote(dir, baseNameOf(p))),
        async (t) => {
          await invoke('sftp_stat', { sessionId, path: t });
          return true;
        },
      );
      if (policy === null) return;
      let skipped = 0;
      for (const p of paths) {
        try {
          const res = await invoke<{ transferIds: string[]; skipped: number }>('sftp_upload', {
            sessionId,
            local: p,
            remote: dir,
            ...(policy === 'none' ? {} : { onExists: policy }),
          });
          skipped += res.skipped;
        } catch (e) {
          notify(`上传失败: ${e}`, 'error');
        }
      }
      if (skipped > 0) notify(`已跳过 ${skipped} 个已存在文件`, 'info');
    })();
  };

  const downloadPathsTo = (paths: string[], dir: string) => {
    void (async () => {
      // 盘符枚举页（dir=''）无确定落点，跳过冲突检测
      const policy =
        dir === ''
          ? 'none'
          : await pickOnExists(
              paths.map((p) => joinRemote(dir, baseNameOf(p))),
              async (t) => (await invoke<{ exists: boolean }>('local_stat', { path: t })).exists,
            );
      if (policy === null) return;
      let skipped = 0;
      for (const p of paths) {
        try {
          const res = await invoke<{ transferIds: string[]; skipped: number }>('sftp_download', {
            sessionId,
            remote: p,
            local: dir,
            ...(policy === 'none' ? {} : { onExists: policy }),
          });
          skipped += res.skipped;
        } catch (e) {
          notify(`下载失败: ${e}`, 'error');
        }
      }
      if (skipped > 0) notify(`已跳过 ${skipped} 个已存在文件`, 'info');
    })();
  };

  const uploadPaths = (paths: string[]) => uploadPathsTo(paths, remotePath);

  const downloadPaths = (paths: string[]) => downloadPathsTo(paths, localPath || '');

  const transferDrop = (toSide: Side) => (paths: string[]) => {
    if (toSide === 'remote') uploadPaths(paths);
    else downloadPaths(paths);
  };

  /** 栏间拖拽落到目录行：进该子目录（批次六 1b） */
  const transferDropInto = (toSide: Side) => (paths: string[], dir: string) => {
    if (toSide === 'remote') uploadPathsTo(paths, dir);
    else downloadPathsTo(paths, dir);
  };

  // ---------- 元操作（mkdir/rename/chmod/move/delete） ----------

  const runPrompt = async () => {
    if (!prompt) return;
    const dir = prompt.side === 'local' ? localPath : remotePath;
    const join = (name: string) => (dir === '/' || dir === '' ? `${dir}${name}` : `${dir}/${name}`);
    try {
      if (prompt.action === 'mkdir') {
        if (prompt.side === 'remote')
          await invoke('sftp_mkdir', { sessionId, path: join(prompt.value) });
        else await invoke('local_mkdir', { path: join(prompt.value) });
      } else if (prompt.action === 'touch') {
        // 新建空文件（批次十一 7）：已存在则后端报错
        if (prompt.side === 'remote')
          await invoke('sftp_touch', { sessionId, path: join(prompt.value) });
        else await invoke('local_touch', { path: join(prompt.value) });
      } else if (prompt.action === 'rename') {
        const one = selEntries(prompt.side)[0];
        if (!one) return;
        if (prompt.side === 'remote') {
          await invoke('sftp_rename', { sessionId, from: one.path, to: join(prompt.value) });
        } else {
          await invoke('local_rename', { from: one.path, to: join(prompt.value) });
        }
      } else if (prompt.action === 'chmod') {
        const mode = parseInt(prompt.value, 8);
        for (const e of selEntries('remote')) {
          await invoke('sftp_chmod', { sessionId, path: e.path, mode });
        }
      } else if (prompt.action === 'move') {
        const target = prompt.value;
        for (const e of selEntries(prompt.side)) {
          const to = `${target.replace(/\/+$/, '')}/${e.name}`;
          if (prompt.side === 'remote') {
            await invoke('sftp_rename', { sessionId, from: e.path, to });
          } else {
            await invoke('local_rename', { from: e.path, to });
          }
        }
      }
    } catch (e) {
      notify(`操作失败: ${e}`, 'error');
    }
    setPrompt(null);
    clearSel();
    if (prompt.side === 'remote') void refreshRemote();
    else void refreshLocal();
  };

  /** 确认后批量删除（目录递归删除，无法恢复；逐项执行，单项失败不阻断其余） */
  const deleteEntries = async (entries: FileEntry[], side: Side) => {
    let ok = 0;
    for (const entry of entries) {
      try {
        if (side === 'remote') await invoke('sftp_delete', { sessionId, path: entry.path });
        else await invoke('local_delete', { path: entry.path });
        ok++;
      } catch (e) {
        notify(`删除 ${entry.name} 失败: ${e}`, 'error');
      }
    }
    if (ok > 0) notify(`已删除 ${ok} 个项目`, 'success');
    clearSel();
    if (side === 'remote') void refreshRemote();
    else void refreshLocal();
  };

  // ---------- 右键菜单（11.4） ----------

  const rowMenu = (side: Side) => (entry: FileEntry, x: number, y: number) => {
    setSel((s) =>
      s.side === side && s.paths.has(entry.path)
        ? s
        : { side, paths: new Set([entry.path]), anchor: entry.path },
    );
    setMenu({ x, y, side });
  };

  /** 复制路径（批次六 2；批次十一 8 改 Tauri 剪贴板插件）：多选逐行拼接 */
  const copyPaths = (side: Side) => {
    const picked = selEntries(side);
    if (picked.length === 0) return;
    void (async () => {
      try {
        await writeText(picked.map((e) => e.path).join('\n'));
        notify(`已复制 ${picked.length} 个路径`, 'success');
      } catch (e) {
        notify(`复制失败: ${e}`, 'error');
      }
    })();
  };

  const menuItems = (side: Side): MenuItem[] => {
    const picked = selEntries(side);
    const n = picked.length;
    const one = n === 1 ? picked[0] : undefined;
    const batch = n > 0;
    if (side === 'remote') {
      return [
        {
          label: '打开',
          icon: '▶',
          disabled: !one,
          onSelect: () => one && openEntry('remote')(one),
        },
        {
          label: n > 1 ? `下载 ${n} 项` : '下载',
          icon: '⬇',
          disabled: !batch,
          onSelect: () => downloadPaths(picked.map((e) => e.path)),
        },
        {
          label: n > 1 ? `复制 ${n} 个路径` : '复制路径',
          icon: '⧉',
          disabled: !batch,
          onSelect: () => copyPaths('remote'),
        },
        'separator',
        {
          label: '重命名…',
          icon: '✎',
          disabled: !one,
          onSelect: () => one && setPrompt({ action: 'rename', side, value: one.name }),
        },
        {
          label: n > 1 ? `移动 ${n} 项到…` : '移动到…',
          icon: '➜',
          disabled: !batch,
          onSelect: () => setPrompt({ action: 'move', side, value: remotePath }),
        },
        {
          label: n > 1 ? `修改 ${n} 项权限…` : '修改权限…',
          icon: '⚙',
          disabled: !batch,
          onSelect: () =>
            setPrompt({ action: 'chmod', side, value: fmtPerms(one?.permissions) || '644' }),
        },
        {
          label: '新建目录…',
          icon: '＋',
          onSelect: () => setPrompt({ action: 'mkdir', side, value: '' }),
        },
        {
          label: '新建文件…',
          icon: '＋',
          onSelect: () => setPrompt({ action: 'touch', side, value: '' }),
        },
        { label: '刷新', icon: '↻', onSelect: () => void refreshRemote() },
        'separator',
        {
          label: n > 1 ? `删除 ${n} 项…` : '删除…',
          icon: '🗑',
          danger: true,
          disabled: !batch,
          onSelect: () => setConfirmDel(picked),
        },
      ];
    }
    return [
      {
        label: '在资源管理器中显示',
        icon: '⬈',
        disabled: !one,
        onSelect: () =>
          one &&
          void invoke('open_in_explorer', { path: one.path }).catch((e) =>
            notify(`打开失败: ${e}`, 'error'),
          ),
      },
      {
        label: n > 1 ? `上传 ${n} 项` : '上传',
        icon: '⬆',
        disabled: !batch,
        onSelect: () => uploadPaths(picked.map((e) => e.path)),
      },
      {
        label: n > 1 ? `复制 ${n} 个路径` : '复制路径',
        icon: '⧉',
        disabled: !batch,
        onSelect: () => copyPaths('local'),
      },
      'separator',
      {
        label: '重命名…',
        icon: '✎',
        disabled: !one,
        onSelect: () => one && setPrompt({ action: 'rename', side, value: one.name }),
      },
      {
        label: n > 1 ? `移动 ${n} 项到…` : '移动到…',
        icon: '➜',
        disabled: !batch,
        onSelect: () => setPrompt({ action: 'move', side, value: localPath }),
      },
      {
        label: '新建目录…',
        icon: '＋',
        onSelect: () => setPrompt({ action: 'mkdir', side, value: '' }),
      },
      {
        label: '新建文件…',
        icon: '＋',
        onSelect: () => setPrompt({ action: 'touch', side, value: '' }),
      },
      { label: '刷新', icon: '↻', onSelect: () => void refreshLocal() },
      'separator',
      {
        label: n > 1 ? `删除 ${n} 项…` : '删除…',
        icon: '🗑',
        danger: true,
        disabled: !batch,
        onSelect: () => setConfirmDel(picked),
      },
    ];
  };

  // ---------- 传输摘要（批次六 5）：完整队列迁至 TransferCenter，此处只留计数 + 入口 ----------

  const transfers = sessionTransfers ?? [];
  const liveCount = transfers.filter(
    (t) => !t.history && (t.state === 'queued' || t.state === 'running' || t.state === 'paused'),
  ).length;
  const failedCount = transfers.filter((t) => !t.history && t.state === 'failed').length;

  // ---------- 路径栏模糊建议（批次六 3）：按草稿父目录拉列表，前缀/包含过滤 ----------

  const fetchSuggestions =
    (side: Side) =>
    async (input: string): Promise<string[]> => {
      const remote = side === 'remote';
      const value = input.trim();
      if (!value) return [];
      // 以最后一个分隔符拆父目录与前缀；Windows 盘符根 C:/ 的父级是其自身
      const norm = remote ? value : value.replace(/\\/g, '/');
      const idx = norm.lastIndexOf('/');
      let parent: string;
      let frag: string;
      if (idx < 0) {
        parent = remote ? '/' : ''; // 本地空父级 = 盘符枚举
        frag = norm;
      } else {
        const head = norm.slice(0, idx);
        parent = remote ? head || '/' : /^[A-Z]:$/i.test(head) ? `${head}/` : head;
        frag = norm.slice(idx + 1);
      }
      try {
        const entries = remote
          ? (await invoke<{ entries: FileEntry[] }>('sftp_list', { sessionId, path: parent }))
              .entries
          : (await invoke<{ entries: FileEntry[]; path: string }>('local_list', { path: parent }))
              .entries;
        const f = frag.toLowerCase();
        const base = parent === '' ? '' : parent.endsWith('/') ? parent : `${parent}/`;
        return entries
          .filter((e) => e.kind === 'dir' || e.kind === 'symlink')
          .filter((e) => {
            const name = e.name.toLowerCase();
            return !f || name.startsWith(f) || name.includes(f);
          })
          .slice(0, 20)
          .map((e) => (parent === '' ? e.path : `${base}${e.name}`));
      } catch {
        return [];
      }
    };

  // ---------- 本地快捷位置（批次六 4；批次十 3：常用路径动态增删，settings KV 持久化） ----------

  const rawFavs = appSettings['sftp.localFavorites'];
  const localFavs: string[] = Array.isArray(rawFavs)
    ? rawFavs.filter((x): x is string => typeof x === 'string' && x.trim() !== '')
    : [];

  const favLabel = (p: string) => {
    const norm = p.replace(/\\/g, '/').replace(/\/+$/, '');
    return norm.split('/').pop() || norm; // 盘符根 C:/ → C:
  };
  const addLocalFav = () => {
    if (!localPath || localFavs.includes(localPath)) return;
    setSetting('sftp.localFavorites', [...localFavs, localPath]);
    notify(`已收藏本地路径: ${localPath}`, 'success');
  };
  const removeLocalFav = (p: string) =>
    setSetting(
      'sftp.localFavorites',
      localFavs.filter((x) => x !== p),
    );

  // 远程收藏（批次十一 6）：与本地收藏同模式，按会话隔离（settings 值 Record<sessionId, string[]>）
  const rawRemoteFavMap = appSettings['sftp.remoteFavorites'];
  const remoteFavMap: Record<string, unknown> =
    rawRemoteFavMap !== null &&
    typeof rawRemoteFavMap === 'object' &&
    !Array.isArray(rawRemoteFavMap)
      ? (rawRemoteFavMap as Record<string, unknown>)
      : {};
  const remoteFavs: string[] = (
    sessionId && Array.isArray(remoteFavMap[sessionId])
      ? (remoteFavMap[sessionId] as unknown[])
      : []
  ).filter((x): x is string => typeof x === 'string' && x.trim() !== '');
  const addRemoteFav = () => {
    if (!sessionId || remoteFavs.includes(remotePath)) return;
    setSetting('sftp.remoteFavorites', {
      ...remoteFavMap,
      [sessionId]: [...remoteFavs, remotePath],
    });
    notify(`已收藏远程路径: ${remotePath}`, 'success');
  };
  const removeRemoteFav = (p: string) => {
    if (!sessionId) return;
    setSetting('sftp.remoteFavorites', {
      ...remoteFavMap,
      [sessionId]: remoteFavs.filter((x) => x !== p),
    });
  };

  const qbtn =
    'rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 hover:text-neutral-200 disabled:opacity-40';
  const localQuickSlots = (
    <div className="flex items-center gap-1 overflow-x-auto border-b border-neutral-800 px-2 py-0.5">
      <button className={qbtn} title="盘符枚举" onClick={() => void navSide('local', '')}>
        此电脑
      </button>
      <button
        className={qbtn}
        title="桌面"
        onClick={() =>
          void invoke<string>('local_desktop_path')
            .then((p) => navSide('local', p))
            .catch((e) => notify(`桌面定位失败: ${e}`, 'error'))
        }
      >
        桌面
      </button>
      {localFavs.map((p) => (
        <button
          key={p}
          className={`${qbtn} max-w-32 truncate`}
          title={`${p}（右键移除）`}
          onClick={() => void navSide('local', p)}
          onContextMenu={(e) => {
            e.preventDefault();
            setFavMenu({ x: e.clientX, y: e.clientY, path: p, side: 'local' });
          }}
        >
          {favLabel(p)}
        </button>
      ))}
      <button
        className={`${qbtn} shrink-0`}
        title={localPath ? `收藏当前路径: ${localPath}` : '先进入一个本地目录再收藏'}
        disabled={!localPath || localFavs.includes(localPath)}
        onClick={addLocalFav}
      >
        ☆ 收藏当前
      </button>
    </div>
  );

  const remoteQuickSlots = (
    <div className="flex items-center gap-1 overflow-x-auto border-b border-neutral-800 px-2 py-0.5">
      {remoteFavs.map((p) => (
        <button
          key={p}
          className={`${qbtn} max-w-32 truncate`}
          title={`${p}（右键移除）`}
          onClick={() => void navSide('remote', p)}
          onContextMenu={(e) => {
            e.preventDefault();
            setFavMenu({ x: e.clientX, y: e.clientY, path: p, side: 'remote' });
          }}
        >
          {favLabel(p)}
        </button>
      ))}
      <button
        className={`${qbtn} shrink-0`}
        title={`收藏当前路径: ${remotePath}`}
        disabled={remoteFavs.includes(remotePath)}
        onClick={addRemoteFav}
      >
        ☆ 收藏当前
      </button>
    </div>
  );

  // ---------- 渲染 ----------

  const localSel = selEntries('local');
  const remoteSel = selEntries('remote');
  const toolBtn = 'rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 disabled:opacity-40';

  return (
    <div
      ref={rootRef}
      className="flex shrink-0 flex-col border-t border-neutral-800 bg-neutral-900 text-xs"
      style={{ height }}
    >
      {handle}
      {/* 工具行 */}
      <div className="flex items-center gap-2 border-b border-neutral-800 px-2 py-1">
        <span className="font-medium text-neutral-300">SFTP</span>
        <label className="flex items-center gap-1 text-neutral-500">
          <input
            type="checkbox"
            checked={followTerm}
            onChange={(e) => setFollowTerm(e.target.checked)}
          />
          跟随终端目录
        </label>
        <button
          className={toolBtn}
          onClick={() => {
            void refreshLocal();
            void refreshRemote();
          }}
        >
          刷新
        </button>
        <button
          className={toolBtn}
          disabled={remoteSel.length !== 1 && localSel.length !== 1}
          onClick={() => {
            const side = remoteSel.length === 1 ? 'remote' : 'local';
            const e = (side === 'remote' ? remoteSel : localSel)[0];
            if (e) setPrompt({ action: 'rename', side, value: e.name });
          }}
        >
          重命名
        </button>
        <button
          className={toolBtn}
          disabled={remoteSel.length === 0}
          onClick={() =>
            setPrompt({
              action: 'chmod',
              side: 'remote',
              value: fmtPerms(remoteSel[0]?.permissions) || '644',
            })
          }
        >
          权限
        </button>
        <button
          className={`${toolBtn} text-red-400`}
          disabled={localSel.length === 0 && remoteSel.length === 0}
          onClick={() => {
            const side = remoteSel.length > 0 ? 'remote' : 'local';
            setConfirmDel(side === 'remote' ? remoteSel : localSel);
          }}
        >
          删除
        </button>
        <button
          className={toolBtn}
          disabled={localSel.length === 0}
          onClick={() => uploadPaths(localSel.map((e) => e.path))}
        >
          上传 →{localSel.length > 1 ? ` (${localSel.length})` : ''}
        </button>
        <button
          className={toolBtn}
          disabled={remoteSel.length === 0}
          onClick={() => downloadPaths(remoteSel.map((e) => e.path))}
        >
          ← 下载{remoteSel.length > 1 ? ` (${remoteSel.length})` : ''}
        </button>
        <button
          className="ml-auto rounded px-1.5 py-0.5 text-neutral-500 hover:bg-neutral-800"
          onClick={() => toggleSftp(tabId)}
          aria-label="close sftp"
        >
          ×
        </button>
      </div>

      {/* 内联输入（mkdir/rename/chmod/move） */}
      {prompt && (
        <div className="flex items-center gap-2 border-b border-neutral-800 bg-neutral-850 px-2 py-1">
          <span className="text-neutral-400">
            {prompt.action === 'mkdir'
              ? `目录名（${prompt.side === 'local' ? '本地' : '远程'}）`
              : prompt.action === 'touch'
                ? `文件名（${prompt.side === 'local' ? '本地' : '远程'}）`
                : prompt.action === 'rename'
                  ? '新名称'
                  : prompt.action === 'move'
                    ? '目标目录'
                    : '权限(八进制)'}
          </span>
          <input
            className="w-56 rounded border border-neutral-700 bg-neutral-800 px-1.5 py-0.5 text-neutral-200 outline-none focus:border-blue-600"
            value={prompt.value}
            autoFocus
            onChange={(e) => setPrompt({ ...prompt, value: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void runPrompt();
              if (e.key === 'Escape') setPrompt(null);
            }}
          />
          <button
            className="rounded bg-blue-700 px-2 py-0.5 text-white"
            onClick={() => void runPrompt()}
          >
            确定
          </button>
          <button className="rounded px-2 py-0.5 text-neutral-400" onClick={() => setPrompt(null)}>
            取消
          </button>
        </div>
      )}

      {/* 删除确认（批量；目录递归删除无法恢复；默认焦点在取消） */}
      {confirmDel && (
        <ConfirmDialog
          title={
            confirmDel.length > 1
              ? `删除 ${confirmDel.length} 个项目？`
              : `删除${confirmDel[0]?.kind === 'dir' ? '目录' : '文件'}“${confirmDel[0]?.name}”？`
          }
          confirmLabel={confirmDel.some((e) => e.kind === 'dir') ? '递归删除' : '删除'}
          onCancel={() => setConfirmDel(null)}
          onConfirm={() => {
            const entries = confirmDel;
            setConfirmDel(null);
            void deleteEntries(entries, sel.side);
          }}
        >
          {confirmDel.length === 1 && (
            <p className="mb-1 break-all font-mono text-neutral-300">{confirmDel[0]?.path}</p>
          )}
          {confirmDel.some((e) => e.kind === 'dir') && (
            <p className="mb-1 text-red-300">包含目录，目录将被递归删除，其中所有内容都会丢失。</p>
          )}
          <p>共 {confirmDel.length} 个项目。此操作无法恢复。</p>
        </ConfirmDialog>
      )}

      {/* 文件右键菜单 */}
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menuItems(menu.side)}
          onClose={() => setMenu(null)}
        />
      )}
      {/* 常用路径 chip 右键（批次十 3 本地；批次十一 6 远程） */}
      {favMenu && (
        <ContextMenu
          x={favMenu.x}
          y={favMenu.y}
          items={[
            { label: '打开', icon: '▶', onSelect: () => void navSide(favMenu.side, favMenu.path) },
            {
              label: '移除收藏',
              icon: '🗑',
              danger: true,
              onSelect: () =>
                favMenu.side === 'local'
                  ? removeLocalFav(favMenu.path)
                  : removeRemoteFav(favMenu.path),
            },
          ]}
          onClose={() => setFavMenu(null)}
        />
      )}

      {/* 覆盖确认（批次十一 1）：整批冲突统一策略，默认聚焦续传；Esc/遮罩=取消整批 */}
      {conflictAsk && (
        <Dialog
          title="目标已存在"
          onClose={() => resolveConflict(null)}
          panelClass="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
        >
          <h2 className="mb-2 text-base font-semibold text-neutral-100">
            目标已存在 {conflictAsk.count} 个文件
          </h2>
          <p className="mb-4 text-xs leading-5 text-neutral-400">
            对本批全部冲突文件统一应用所选策略；取消则不开始传输。
          </p>
          <div className="flex justify-end gap-2">
            <button
              data-autofocus
              type="button"
              className="rounded bg-blue-700 px-3 py-1 text-white hover:bg-blue-600"
              onClick={() => resolveConflict('resume')}
            >
              续传
            </button>
            <button
              type="button"
              className="rounded px-3 py-1 text-neutral-300 hover:bg-neutral-800"
              onClick={() => resolveConflict('overwrite')}
            >
              覆盖
            </button>
            <button
              type="button"
              className="rounded px-3 py-1 text-neutral-300 hover:bg-neutral-800"
              onClick={() => resolveConflict('skip')}
            >
              跳过
            </button>
            <button
              type="button"
              className="rounded px-3 py-1 text-neutral-300 hover:bg-neutral-800"
              onClick={() => resolveConflict('rename')}
            >
              自动重命名
            </button>
          </div>
        </Dialog>
      )}

      {/* 双栏 */}
      <div className="flex min-h-0 flex-1">
        <div className="flex min-h-0 flex-1 flex-col">
          <FilePane
            side="local"
            entries={localEntries}
            path={localPath}
            loading={localLoading}
            canBack={localHist.back.length > 0}
            canFwd={localHist.fwd.length > 0}
            sel={sel.side === 'local' ? sel.paths : new Set()}
            onRowClick={rowClick('local')}
            onOpen={openEntry('local')}
            onNavigate={(p) => navSide('local', p)}
            onBack={() => void navBack('local')}
            onFwd={() => void navFwd('local')}
            onUp={() => void navSide('local', parentDir(localPath, false))}
            onRefresh={() => void refreshLocal()}
            onDropEntries={transferDrop('local')}
            onDropEntriesInto={transferDropInto('local')}
            fetchSuggestions={fetchSuggestions('local')}
            dropActive={html5DragSide === 'local'}
            onDropHover={(v) => setHtml5DragSide(v ? 'local' : null)}
            quickSlots={localQuickSlots}
            onRowMenu={rowMenu('local')}
            onClearSel={clearSel}
            onArrowSelect={arrowSelect('local')}
            onRowKey={rowKey('local')}
            onSelectAll={() =>
              setSel({
                side: 'local',
                paths: new Set(localEntries.map((e) => e.path)),
                anchor: localEntries[0]?.path ?? null,
              })
            }
          />
        </div>
        <div className="w-px bg-neutral-800" />
        <div className="flex min-h-0 flex-1 flex-col">
          <FilePane
            side="remote"
            entries={remoteEntries}
            path={remotePath}
            loading={remoteLoading}
            canBack={remoteHist.back.length > 0}
            canFwd={remoteHist.fwd.length > 0}
            sel={sel.side === 'remote' ? sel.paths : new Set()}
            onRowClick={rowClick('remote')}
            onOpen={openEntry('remote')}
            onNavigate={(p) => navSide('remote', p)}
            onBack={() => void navBack('remote')}
            onFwd={() => void navFwd('remote')}
            onUp={() => void navSide('remote', parentDir(remotePath, true))}
            onRefresh={() => void refreshRemote()}
            onDropEntries={transferDrop('remote')}
            onDropEntriesInto={transferDropInto('remote')}
            fetchSuggestions={fetchSuggestions('remote')}
            dropActive={html5DragSide === 'remote'}
            onDropHover={(v) => setHtml5DragSide(v ? 'remote' : null)}
            quickSlots={remoteQuickSlots}
            onRowMenu={rowMenu('remote')}
            onClearSel={clearSel}
            onArrowSelect={arrowSelect('remote')}
            onRowKey={rowKey('remote')}
            onSelectAll={() =>
              setSel({
                side: 'remote',
                paths: new Set(remoteEntries.map((e) => e.path)),
                anchor: remoteEntries[0]?.path ?? null,
              })
            }
          />
        </div>
      </div>

      {/* 传输摘要（批次六 5）：完整队列/逐任务控制在 TransferCenter 抽屉 */}
      <div className="flex items-center gap-2 border-t border-neutral-800 px-2 py-1 text-neutral-500">
        <span>
          传输
          {liveCount > 0 && `：${liveCount} 个进行中`}
          {failedCount > 0 && <span className="text-red-400">，{failedCount} 个失败</span>}
          {liveCount === 0 && failedCount === 0 && '：空闲'}
        </span>
        <button
          className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800"
          onClick={() => useTransferStore.getState().setOpen(true)}
        >
          打开传输管理
        </button>
      </div>
    </div>
  );
}
