import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Sidebar } from './components/Sidebar';
import { TabBar } from './components/TabBar';
import { TunnelPanel } from './components/TunnelPanel';
import { CommandPalette } from './components/CommandPalette';
import { ConnectDialog } from './components/ConnectDialog';
import { HostKeyDialog } from './components/HostKeyDialog';
import { KiDialog } from './components/KiDialog';
import { SplitTree } from './components/SplitTree';
import { useAppStore } from './state/app-store';

export function App() {
  const [version, setVersion] = useState('…');
  const tabs = useAppStore((s) => s.tabs);
  const activeId = useAppStore((s) => s.activeId);
  const openConnect = useAppStore((s) => s.openConnect);
  const toggleSidebar = useAppStore((s) => s.toggleSidebar);
  const toggleTunnelPanel = useAppStore((s) => s.toggleTunnelPanel);
  const subscribeTunnels = useAppStore((s) => s.subscribeTunnels);
  const togglePalette = useAppStore((s) => s.togglePalette);
  const paletteOpen = useAppStore((s) => s.paletteOpen);
  const notice = useAppStore((s) => s.notice);

  useEffect(() => {
    subscribeTunnels();
  }, [subscribeTunnels]);

  // 全局命令面板快捷键（xterm 焦点下 window keydown 仍可收到）
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.ctrlKey && e.shiftKey && (e.key === 'P' || e.key === 'p')) {
        e.preventDefault();
        togglePalette();
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [togglePalette]);

  useEffect(() => {
    // 纯浏览器（vite 诊断）下 invoke 不可用
    invoke<string>('app_version')
      .then(setVersion)
      .catch(() => setVersion('dev'));
  }, []);

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-neutral-800 px-3 py-1.5 text-xs text-neutral-400">
        <span className="flex items-center gap-2">
          <button
            className="rounded px-1 hover:bg-neutral-800"
            onClick={toggleSidebar}
            title="会话侧栏"
          >
            ☰
          </button>
          <span className="font-semibold text-neutral-200">mySSH</span>
        </span>
        <span className="flex items-center gap-2">
          <button
            className="rounded px-1 hover:bg-neutral-800"
            onClick={toggleTunnelPanel}
            title="隧道面板"
          >
            ⇄
          </button>
          <span>v{version} · M2</span>
        </span>
      </header>
      <TabBar />
      <main className="flex min-h-0 flex-1">
        <Sidebar />
        <div className="relative h-full min-w-0 flex-1">
          {tabs.length === 0 ? (
            <div className="flex h-full items-center justify-center text-sm text-neutral-500">
              <button
                className="rounded border border-neutral-700 px-4 py-2 hover:bg-neutral-800"
                onClick={() => openConnect()}
              >
                ＋ 新建 SSH 连接
              </button>
            </div>
          ) : (
            tabs.map((t) => (
              <div
                key={t.id}
                className="h-full w-full"
                style={{ display: t.id === activeId ? undefined : 'none' }}
              >
                <SplitTree tab={t} node={t.layout} visible={t.id === activeId} />
              </div>
            ))
          )}
        </div>
      </main>
      <TunnelPanel />
      <ConnectDialog />
      <HostKeyDialog />
      <KiDialog />
      {paletteOpen && <CommandPalette />}
      {notice && (
        <div className="fixed bottom-10 left-1/2 z-40 -translate-x-1/2 rounded bg-neutral-800 px-4 py-2 text-xs text-neutral-200 shadow-lg">
          {notice}
        </div>
      )}
    </div>
  );
}
