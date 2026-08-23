import { useCallback, useEffect, useRef, useState } from 'react';
import { Channel, invoke } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { useAppStore } from '../state/app-store';
import type { FileEntry, TransferView } from '../term/types';

/** 双栏 SFTP 面板：左本地 / 右远程，拖拽互传 + 队列进度 + 终端 cwd 联动（OSC 7）。
 *  远端操作命令见 crates/app/src/sftp.rs；传输速率由后端差分下发。 */

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
  side: 'local' | 'remote';
  entries: FileEntry[];
  path: string;
  loading: boolean;
  selected: string | null;
  onSelect: (path: string) => void;
  onOpen: (e: FileEntry) => void;
  onUp: () => void;
  onDropEntries: (paths: string[]) => void;
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
      <div className="flex items-center gap-1 border-b border-neutral-800 px-2 py-1 text-xs text-neutral-500">
        <button className="rounded px-1 hover:bg-neutral-800" onClick={p.onUp} title="上级目录">
          ↑
        </button>
        <span className="truncate font-mono text-neutral-400" title={p.path}>
          {p.path || (p.side === 'local' ? '此电脑' : '/')}
        </span>
        {p.loading && <span className="ml-auto animate-pulse">…</span>}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto">
        {p.entries.map((e) => (
          <div
            key={e.path}
            draggable
            onDragStart={(ev) =>
              ev.dataTransfer.setData(
                'application/x-myssh-entry',
                JSON.stringify({ side: p.side, paths: [e.path] }),
              )
            }
            onClick={() => p.onSelect(e.path)}
            onDoubleClick={() => p.onOpen(e)}
            className={`flex cursor-pointer items-center gap-2 px-2 py-0.5 text-xs ${
              p.selected === e.path
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
  const [localEntries, setLocalEntries] = useState<FileEntry[]>([]);
  const [remoteEntries, setRemoteEntries] = useState<FileEntry[]>([]);
  const [localLoading, setLocalLoading] = useState(false);
  const [remoteLoading, setRemoteLoading] = useState(false);
  const [selected, setSelected] = useState<{ side: 'local' | 'remote'; path: string } | null>(null);
  const [transfers, setTransfers] = useState<TransferView[]>([]);
  const [followTerm, setFollowTerm] = useState(true);
  const [prompt, setPrompt] = useState<{
    action: 'mkdir' | 'rename' | 'chmod';
    side: 'local' | 'remote';
    value: string;
  } | null>(null);
  const remotePaneRef = useRef<HTMLDivElement>(null);

  const refreshLocal = useCallback(
    async (path?: string) => {
      const target = path ?? localPath;
      setLocalLoading(true);
      try {
        const res = await invoke<{ entries: FileEntry[]; path: string }>('local_list', {
          path: target,
        });
        setLocalEntries(res.entries);
        if (res.path !== target) setLocalPath(res.path);
      } catch (e) {
        notify(`本地目录读取失败: ${e}`);
      } finally {
        setLocalLoading(false);
      }
    },
    [localPath, notify],
  );

  const refreshRemote = useCallback(
    async (path?: string) => {
      if (!sessionId) return;
      const target = path ?? remotePath;
      setRemoteLoading(true);
      try {
        const res = await invoke<{ entries: FileEntry[] }>('sftp_list', {
          sessionId,
          path: target,
        });
        setRemoteEntries(res.entries);
        setRemotePath(target);
      } catch (e) {
        notify(`远程目录读取失败: ${e}`);
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
    events.onmessage = (f) => setTransfers(f.transfers);
    void invoke('transfer_subscribe', { sessionId, events });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  // 终端 cwd 跟随（OSC 7）
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
              notify(`上传失败: ${e}`),
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

  const openEntry = (side: 'local' | 'remote') => (e: FileEntry) => {
    if (e.kind === 'dir') {
      if (side === 'local') {
        setLocalPath(e.path);
        void refreshLocal(e.path);
      } else {
        void refreshRemote(e.path);
      }
    } else if (side === 'remote' && e.kind === 'file') {
      // 双击远程文件 = 直编（下载临时区 + 监听回传 + 系统默认编辑器）
      void (async () => {
        try {
          const res = await invoke<{ localPath: string }>('sftp_edit_open', {
            sessionId,
            remote: e.path,
          });
          await invoke('open_local', { path: res.localPath });
          notify(`已打开编辑: ${e.name}（保存即回传）`);
        } catch (err) {
          notify(`编辑打开失败: ${err}`);
        }
      })();
    }
  };

  const transferDrop = (toSide: 'local' | 'remote') => (paths: string[]) => {
    for (const p of paths) {
      if (toSide === 'remote') {
        void invoke('sftp_upload', { sessionId, local: p, remote: remotePath }).catch((e) =>
          notify(`上传失败: ${e}`),
        );
      } else {
        void invoke('sftp_download', { sessionId, remote: p, local: localPath || '' }).catch((e) =>
          notify(`下载失败: ${e}`),
        );
      }
    }
  };

  const runPrompt = async () => {
    if (!prompt) return;
    const sel = selected;
    try {
      if (prompt.action === 'mkdir') {
        const base = prompt.side === 'local' ? localPath : remotePath;
        if (prompt.side === 'remote') {
          await invoke('sftp_mkdir', { sessionId, path: `${base}/${prompt.value}` });
          void refreshRemote();
        } else {
          await invoke('local_mkdir', { path: `${base}/${prompt.value}` });
          void refreshLocal();
        }
      } else if (prompt.action === 'rename' && sel) {
        const dir = prompt.side === 'local' ? localPath : remotePath;
        await invoke('sftp_rename', {
          sessionId,
          from: sel.path,
          to: `${dir === '/' ? '' : dir}/${prompt.value}`,
        });
        void refreshRemote();
      } else if (prompt.action === 'chmod' && sel) {
        await invoke('sftp_chmod', { sessionId, path: sel.path, mode: parseInt(prompt.value, 8) });
        void refreshRemote();
      }
    } catch (e) {
      notify(`操作失败: ${e}`);
    }
    setPrompt(null);
  };

  const deleteSelected = async () => {
    if (!selected || selected.side !== 'remote') return;
    try {
      await invoke('sftp_delete', { sessionId, path: selected.path });
      setSelected(null);
      void refreshRemote();
      notify('已删除');
    } catch (e) {
      notify(`删除失败: ${e}`);
    }
  };

  const selEntry = (side: 'local' | 'remote'): FileEntry | undefined => {
    if (!selected || selected.side !== side) return undefined;
    return (side === 'local' ? localEntries : remoteEntries).find((e) => e.path === selected.path);
  };

  return (
    <div className="flex h-64 flex-col border-t border-neutral-800 bg-neutral-900 text-xs">
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
          className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800"
          onClick={() => {
            void refreshLocal();
            void refreshRemote();
          }}
        >
          刷新
        </button>
        <button
          className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800"
          onClick={() => setPrompt({ action: 'mkdir', side: 'remote', value: '' })}
        >
          新建目录
        </button>
        <button
          className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 disabled:opacity-40"
          disabled={!selEntry('remote')}
          onClick={() => {
            const e = selEntry('remote');
            if (e) setPrompt({ action: 'rename', side: 'remote', value: e.name });
          }}
        >
          重命名
        </button>
        <button
          className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 disabled:opacity-40"
          disabled={!selEntry('remote')}
          onClick={() => {
            const e = selEntry('remote');
            if (e)
              setPrompt({
                action: 'chmod',
                side: 'remote',
                value: fmtPerms(e.permissions) || '644',
              });
          }}
        >
          权限
        </button>
        <button
          className="rounded px-1.5 py-0.5 text-red-400 hover:bg-neutral-800 disabled:opacity-40"
          disabled={!selEntry('remote')}
          onClick={() => void deleteSelected()}
        >
          删除
        </button>
        <button
          className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 disabled:opacity-40"
          disabled={!selEntry('local') || selEntry('local')?.kind !== 'file'}
          onClick={() => {
            const e = selEntry('local');
            if (e)
              void invoke('sftp_upload', { sessionId, local: e.path, remote: remotePath }).catch(
                (err) => notify(`上传失败: ${err}`),
              );
          }}
        >
          上传 →
        </button>
        <button
          className="rounded px-1.5 py-0.5 text-neutral-400 hover:bg-neutral-800 disabled:opacity-40"
          disabled={!selEntry('remote') || selEntry('remote')?.kind !== 'file'}
          onClick={() => {
            const e = selEntry('remote');
            if (e)
              void invoke('sftp_download', {
                sessionId,
                remote: e.path,
                local: localPath || '',
              }).catch((err) => notify(`下载失败: ${err}`));
          }}
        >
          ← 下载
        </button>
        <button
          className="ml-auto rounded px-1.5 py-0.5 text-neutral-500 hover:bg-neutral-800"
          onClick={() => toggleSftp(tabId)}
          aria-label="close sftp"
        >
          ×
        </button>
      </div>

      {/* 内联输入（mkdir/rename/chmod） */}
      {prompt && (
        <div className="flex items-center gap-2 border-b border-neutral-800 bg-neutral-850 px-2 py-1">
          <span className="text-neutral-400">
            {prompt.action === 'mkdir'
              ? '目录名'
              : prompt.action === 'rename'
                ? '新名称'
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

      {/* 双栏 */}
      <div className="flex min-h-0 flex-1">
        <FilePane
          side="local"
          entries={localEntries}
          path={localPath}
          loading={localLoading}
          selected={selected?.side === 'local' ? selected.path : null}
          onSelect={(p) => setSelected({ side: 'local', path: p })}
          onOpen={openEntry('local')}
          onUp={() => {
            const up = parentDir(localPath, false);
            setLocalPath(up);
            void refreshLocal(up);
          }}
          onDropEntries={transferDrop('local')}
        />
        <div className="w-px bg-neutral-800" />
        <div ref={remotePaneRef} className="flex min-h-0 flex-1 flex-col">
          <FilePane
            side="remote"
            entries={remoteEntries}
            path={remotePath}
            loading={remoteLoading}
            selected={selected?.side === 'remote' ? selected.path : null}
            onSelect={(p) => setSelected({ side: 'remote', path: p })}
            onOpen={openEntry('remote')}
            onUp={() => void refreshRemote(parentDir(remotePath, true))}
            onDropEntries={transferDrop('remote')}
          />
        </div>
      </div>

      {/* 传输队列条 */}
      {transfers.length > 0 && (
        <div className="max-h-20 overflow-y-auto border-t border-neutral-800 px-2 py-1">
          {transfers.map((t) => {
            const pct = t.bytesTotal > 0 ? Math.min(100, (t.bytesDone / t.bytesTotal) * 100) : 0;
            const name = t.remote.split('/').pop() || t.remote;
            return (
              <div key={t.id} className="flex items-center gap-2 py-0.5 text-neutral-400">
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
                {!t.history && t.state === 'running' && (
                  <>
                    <button
                      className="rounded px-1 hover:bg-neutral-800"
                      title="暂停"
                      onClick={() => void invoke('transfer_pause', { sessionId, transferId: t.id })}
                    >
                      ⏸
                    </button>
                    <button
                      className="rounded px-1 hover:bg-neutral-800"
                      title="取消"
                      onClick={() =>
                        void invoke('transfer_cancel', { sessionId, transferId: t.id })
                      }
                    >
                      ✕
                    </button>
                  </>
                )}
                {!t.history && t.state === 'queued' && (
                  <button
                    className="rounded px-1 hover:bg-neutral-800"
                    title="取消"
                    onClick={() => void invoke('transfer_cancel', { sessionId, transferId: t.id })}
                  >
                    ✕
                  </button>
                )}
                {!t.history && t.state === 'paused' && (
                  <>
                    <button
                      className="rounded px-1 hover:bg-neutral-800"
                      title="继续"
                      onClick={() =>
                        void invoke('transfer_resume', { sessionId, transferId: t.id })
                      }
                    >
                      ▶
                    </button>
                    <button
                      className="rounded px-1 hover:bg-neutral-800"
                      title="取消"
                      onClick={() =>
                        void invoke('transfer_cancel', { sessionId, transferId: t.id })
                      }
                    >
                      ✕
                    </button>
                  </>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
