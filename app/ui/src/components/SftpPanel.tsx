import { useCallback, useEffect, useRef, useState } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useAppStore } from '../state/app-store';
import type { FileEntry, TransferView } from '../term/types';
import { ConfirmDialog } from './ConfirmDialog';
import { ContextMenu, type MenuItem } from './ContextMenu';
import { PathBar } from './PathBar';
import {
  EMPTY_NAV_HIST,
  navBack as histBack,
  navDropBack,
  navDropFwd,
  navFwd as histFwd,
  navPush,
  type NavHist,
} from './nav-hist';
import { usePanelHeight } from './panel-height';

/** 双栏 SFTP 面板：左本地 / 右远程，拖拽互传 + 队列进度 + 终端 cwd 联动（OSC 7）。
 *  批次五：可编辑路径栏（前进/后退历史）、多选与批量操作、失败任务重试、
 *  本地文件操作、面板拖拽调高。远端操作命令见 crates/app/src/sftp.rs。 */

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
  onRowMenu: (e: FileEntry, x: number, y: number) => void;
  onClearSel: () => void;
  onSelectAll: () => void;
}

function FilePane(p: PaneProps) {
  return (
    <div
      className="flex min-h-0 flex-1 flex-col"
      onDragOver={(e) => e.preventDefault()}
      onDrop={(e) => {
        e.preventDefault();
        const raw = e.dataTransfer.getData('application/x-myssh-entry');
        if (raw) {
          const src = JSON.parse(raw) as { side: string; paths: string[] };
          if (src.side !== p.side) p.onDropEntries(src.paths);
        }
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
      />
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
        {p.entries.map((e) => (
          <div
            key={e.path}
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
            onClick={(ev) => p.onRowClick(e, ev)}
            onDoubleClick={() => p.onOpen(e)}
            onContextMenu={(ev) => {
              ev.preventDefault();
              p.onRowMenu(e, ev.clientX, ev.clientY);
            }}
            className={`flex cursor-pointer items-center gap-2 px-2 py-0.5 text-xs ${
              p.sel.has(e.path)
                ? 'bg-neutral-700 text-neutral-100'
                : 'text-neutral-300 hover:bg-neutral-800'
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
              <span className="w-12 shrink-0 text-right font-mono text-neutral-600">
                {fmtPerms(e.permissions)}
              </span>
            )}
          </div>
        ))}
        {!p.loading && p.entries.length === 0 && (
          <div className="px-3 py-4 text-xs text-neutral-600">（空目录）</div>
        )}
      </div>
    </div>
  );
}

export function SftpPanel({ tabId }: { tabId: string }) {
  const tabs = useAppStore((s) => s.tabs);
  const toggleSftp = useAppStore((s) => s.toggleSftp);
  const notify = useAppStore((s) => s.notify);
  const tab = tabs.find((t) => t.id === tabId);
  const sessionId = tab?.target.kind === 'session' ? tab.target.sessionId : null;
  const pane = tab ? tab.panes[tab.activePaneId] : null;

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
  const [transfers, setTransfers] = useState<TransferView[]>([]);
  const [followTerm, setFollowTerm] = useState(true);
  const [prompt, setPrompt] = useState<{
    action: 'mkdir' | 'rename' | 'chmod' | 'move';
    side: Side;
    value: string;
  } | null>(null);
  /** 待确认删除的条目（11.2 批量；目录递归删除，无法恢复） */
  const [confirmDel, setConfirmDel] = useState<FileEntry[] | null>(null);
  const [menu, setMenu] = useState<{ x: number; y: number; side: Side } | null>(null);
  const [showErr, setShowErr] = useState<Set<string>>(new Set());
  const remotePaneRef = useRef<HTMLDivElement>(null);
  const { height, handle } = usePanelHeight('sftp.height', 256);

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

  const refreshRemote = useCallback(
    async (path?: string): Promise<string | null> => {
      if (!sessionId) return null;
      const target = path ?? remotePath;
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
        notify(`远程目录读取失败: ${e}`, 'error');
        return null;
      } finally {
        setRemoteLoading(false);
      }
    },
    [sessionId, remotePath, notify],
  );

  // 初始加载 + 传输订阅（微任务推迟 setState，避开 effect 内同步渲染级联）
  useEffect(() => {
    if (!sessionId) return;
    const cwd = pane?.session.cwd;
    queueMicrotask(() => {
      void refreshRemote(cwd ?? '/');
      void refreshLocal('');
    });
    const events = new Channel<{ transfers: TransferView[] }>();
    // 12.2 状态栏：顺带发布全局活跃传输数（复用既有订阅，无新增轮询）；
    // 只数本次运行的非终态任务，history 帧不计
    events.onmessage = (f) => {
      setTransfers(f.transfers);
      useAppStore
        .getState()
        .setTransferActive(
          f.transfers.filter(
            (t) =>
              !t.history && (t.state === 'queued' || t.state === 'running' || t.state === 'paused'),
          ).length,
        );
    };
    void invoke('transfer_subscribe', { sessionId, events });
    return () => {
      useAppStore.getState().setTransferActive(null);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  // 终端 cwd 跟随（OSC 7；非用户导航 → 不入历史栈）
  useEffect(() => {
    if (!followTerm || !sessionId) return;
    const timer = setInterval(() => {
      const cwd = pane?.session.cwd;
      if (cwd && cwd !== remotePath) void refreshRemote(cwd);
    }, 1000);
    return () => clearInterval(timer);
  }, [followTerm, sessionId, pane, remotePath, refreshRemote]);

  // OS 文件拖入：落在远程栏 → 上传
  useEffect(() => {
    if (!sessionId) return;
    let unlisten: (() => void) | null = null;
    void getCurrentWindow()
      .onDragDropEvent((ev) => {
        if (ev.payload.type !== 'drop') return;
        const rect = remotePaneRef.current?.getBoundingClientRect();
        if (!rect) return;
        const { x, y } = ev.payload.position;
        if (x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom) {
          for (const p of ev.payload.paths) {
            void invoke('sftp_upload', { sessionId, local: p, remote: remotePath }).catch((e) =>
              notify(`上传失败: ${e}`, 'error'),
            );
          }
        }
      })
      .then((u) => {
        unlisten = u;
      });
    return () => unlisten?.();
  }, [sessionId, remotePath, notify]);

  if (!sessionId) {
    return (
      <div className="border-t border-neutral-800 bg-neutral-900 px-3 py-2 text-xs text-neutral-500">
        SFTP 仅支持存储档案会话（内联连接无档案可解析凭据）
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

  // ---------- 传输 ----------

  const uploadPaths = (paths: string[]) => {
    for (const p of paths) {
      void invoke('sftp_upload', { sessionId, local: p, remote: remotePath }).catch((e) =>
        notify(`上传失败: ${e}`, 'error'),
      );
    }
  };

  const downloadPaths = (paths: string[]) => {
    for (const p of paths) {
      void invoke('sftp_download', { sessionId, remote: p, local: localPath || '' }).catch((e) =>
        notify(`下载失败: ${e}`, 'error'),
      );
    }
  };

  const transferDrop = (toSide: Side) => (paths: string[]) => {
    if (toSide === 'remote') uploadPaths(paths);
    else downloadPaths(paths);
  };

  // ---------- 元操作（mkdir/rename/chmod/move/delete） ----------

  const runPrompt = async () => {
    if (!prompt) return;
    const dir = prompt.side === 'local' ? localPath : remotePath;
    const join = (name: string) => (dir === '/' || dir === '' ? `${dir}${name}` : `${dir}/${name}`);
    try {
      if (prompt.action === 'mkdir') {
        if (prompt.side === 'remote') await invoke('sftp_mkdir', { sessionId, path: join(prompt.value) });
        else await invoke('local_mkdir', { path: join(prompt.value) });
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

  const menuItems = (side: Side): MenuItem[] => {
    const picked = selEntries(side);
    const n = picked.length;
    const one = n === 1 ? picked[0] : undefined;
    const batch = n > 0;
    if (side === 'remote') {
      return [
        {
          label: '打开',
          disabled: !one,
          onSelect: () => one && openEntry('remote')(one),
        },
        {
          label: n > 1 ? `下载 ${n} 项` : '下载',
          disabled: !batch,
          onSelect: () => downloadPaths(picked.map((e) => e.path)),
        },
        'separator',
        {
          label: '重命名…',
          disabled: !one,
          onSelect: () => one && setPrompt({ action: 'rename', side, value: one.name }),
        },
        {
          label: n > 1 ? `移动 ${n} 项到…` : '移动到…',
          disabled: !batch,
          onSelect: () => setPrompt({ action: 'move', side, value: remotePath }),
        },
        {
          label: n > 1 ? `修改 ${n} 项权限…` : '修改权限…',
          disabled: !batch,
          onSelect: () =>
            setPrompt({ action: 'chmod', side, value: fmtPerms(one?.permissions) || '644' }),
        },
        {
          label: '新建目录…',
          onSelect: () => setPrompt({ action: 'mkdir', side, value: '' }),
        },
        { label: '刷新', onSelect: () => void refreshRemote() },
        'separator',
        {
          label: n > 1 ? `删除 ${n} 项…` : '删除…',
          danger: true,
          disabled: !batch,
          onSelect: () => setConfirmDel(picked),
        },
      ];
    }
    return [
      {
        label: '在资源管理器中显示',
        disabled: !one,
        onSelect: () =>
          one &&
          void invoke('open_in_explorer', { path: one.path }).catch((e) =>
            notify(`打开失败: ${e}`, 'error'),
          ),
      },
      {
        label: n > 1 ? `上传 ${n} 项` : '上传',
        disabled: !batch,
        onSelect: () => uploadPaths(picked.map((e) => e.path)),
      },
      'separator',
      {
        label: '重命名…',
        disabled: !one,
        onSelect: () => one && setPrompt({ action: 'rename', side, value: one.name }),
      },
      {
        label: n > 1 ? `移动 ${n} 项到…` : '移动到…',
        disabled: !batch,
        onSelect: () => setPrompt({ action: 'move', side, value: localPath }),
      },
      {
        label: '新建目录…',
        onSelect: () => setPrompt({ action: 'mkdir', side, value: '' }),
      },
      { label: '刷新', onSelect: () => void refreshLocal() },
      'separator',
      {
        label: n > 1 ? `删除 ${n} 项…` : '删除…',
        danger: true,
        disabled: !batch,
        onSelect: () => setConfirmDel(picked),
      },
    ];
  };

  // ---------- 传输队列条（11.3） ----------

  const tcmd = (cmd: string, extra: Record<string, unknown> = {}) =>
    void invoke(cmd, { sessionId, ...extra }).catch((e) => notify(`操作失败: ${e}`, 'error'));

  const openDirOf = (t: TransferView, which: 'src' | 'dst') => {
    // upload: src=local 文件, dst=remote 目录；download: src=remote, dst=local 文件
    if (t.direction === 'upload') {
      if (which === 'src') {
        void invoke('open_in_explorer', { path: t.local }).catch((e) =>
          notify(`打开失败: ${e}`, 'error'),
        );
      } else {
        void navSide('remote', parentDir(t.remote, true));
      }
    } else {
      if (which === 'src') {
        void navSide('remote', parentDir(t.remote, true));
      } else {
        void invoke('open_in_explorer', { path: t.local }).catch((e) =>
          notify(`打开失败: ${e}`, 'error'),
        );
      }
    }
  };

  const transferRow = (t: TransferView) => {
    const pct = t.bytesTotal > 0 ? Math.min(100, (t.bytesDone / t.bytesTotal) * 100) : 0;
    const name = t.remote.split('/').pop() || t.remote;
    const failed = !t.history && t.state === 'failed';
    return (
      <div key={t.id}>
        <div className="flex items-center gap-2 py-0.5 text-neutral-400">
          <span>{t.direction === 'upload' ? '⬆' : '⬇'}</span>
          <span className="w-40 truncate" title={`${t.remote}\n${t.error ?? ''}`}>
            {name}
          </span>
          <div className="h-1.5 w-32 overflow-hidden rounded bg-neutral-800">
            <div
              className={`h-full ${t.state === 'failed' ? 'bg-red-600' : t.state === 'done' ? 'bg-green-600' : 'bg-blue-600'}`}
              style={{ width: `${pct}%` }}
            />
          </div>
          <span className="w-16 text-right">{pct.toFixed(0)}%</span>
          <span className="w-20 text-right text-neutral-500">
            {t.state === 'running' ? `${fmtSize(t.rate ?? 0)}/s` : t.state}
          </span>
          {!t.history && (t.state === 'running' || t.state === 'queued') && (
            <>
              {t.state === 'running' && (
                <button
                  className="rounded px-1 hover:bg-neutral-800"
                  title="暂停"
                  onClick={() => tcmd('transfer_pause', { transferId: t.id })}
                >
                  ⏸
                </button>
              )}
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="取消"
                onClick={() => tcmd('transfer_cancel', { transferId: t.id })}
              >
                ✕
              </button>
            </>
          )}
          {!t.history && t.state === 'paused' && (
            <>
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="继续"
                onClick={() => tcmd('transfer_resume', { transferId: t.id })}
              >
                ▶
              </button>
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="取消"
                onClick={() => tcmd('transfer_cancel', { transferId: t.id })}
              >
                ✕
              </button>
            </>
          )}
          {failed && (
            <>
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="重试（从断点续传）"
                onClick={() => tcmd('transfer_retry', { transferId: t.id })}
              >
                ↻
              </button>
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="查看错误"
                onClick={() =>
                  setShowErr((s) => {
                    const next = new Set(s);
                    if (next.has(t.id)) next.delete(t.id);
                    else next.add(t.id);
                    return next;
                  })
                }
              >
                ⓘ
              </button>
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="打开来源目录"
                onClick={() => openDirOf(t, 'src')}
              >
                ⌂⇧
              </button>
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="打开目标目录"
                onClick={() => openDirOf(t, 'dst')}
              >
                ⌂⇩
              </button>
              <button
                className="rounded px-1 hover:bg-neutral-800"
                title="从队列移除"
                onClick={() => tcmd('transfer_remove', { transferId: t.id })}
              >
                🗑
              </button>
            </>
          )}
        </div>
        {failed && showErr.has(t.id) && t.error && (
          <div className="ml-6 break-all py-0.5 text-red-400">{t.error}</div>
        )}
      </div>
    );
  };

  const live = transfers.filter((t) => !t.history);
  const qbtn = 'rounded px-1.5 py-0.5 text-neutral-500 hover:bg-neutral-800';

  // ---------- 渲染 ----------

  const localSel = selEntries('local');
  const remoteSel = selEntries('remote');
  const toolBtn = 'rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 disabled:opacity-40';

  return (
    <div
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
        <ContextMenu x={menu.x} y={menu.y} items={menuItems(menu.side)} onClose={() => setMenu(null)} />
      )}

      {/* 双栏 */}
      <div className="flex min-h-0 flex-1">
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
          onRowMenu={rowMenu('local')}
          onClearSel={clearSel}
          onSelectAll={() =>
            setSel({
              side: 'local',
              paths: new Set(localEntries.map((e) => e.path)),
              anchor: localEntries[0]?.path ?? null,
            })
          }
        />
        <div className="w-px bg-neutral-800" />
        <div ref={remotePaneRef} className="flex min-h-0 flex-1 flex-col">
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
            onRowMenu={rowMenu('remote')}
            onClearSel={clearSel}
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

      {/* 传输队列条（11.3：失败重试/队列管理） */}
      {transfers.length > 0 && (
        <div className="max-h-24 overflow-y-auto border-t border-neutral-800 px-2 py-1">
          <div className="flex items-center gap-2 py-0.5 text-neutral-500">
            <span>传输（{live.length}）</span>
            <button className={qbtn} title="全部暂停" onClick={() => tcmd('transfer_pause_all')}>
              全部暂停
            </button>
            <button className={qbtn} title="全部继续" onClick={() => tcmd('transfer_resume_all')}>
              全部继续
            </button>
            <button
              className={qbtn}
              title="清除已完成"
              onClick={() => tcmd('transfer_clear', { filter: 'done' })}
            >
              清除已完成
            </button>
            <button
              className={qbtn}
              title="清除失败"
              onClick={() => tcmd('transfer_clear', { filter: 'failed' })}
            >
              清除失败
            </button>
          </div>
          {transfers.map(transferRow)}
        </div>
      )}
    </div>
  );
}
