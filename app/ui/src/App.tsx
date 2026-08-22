import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Sidebar } from './components/Sidebar';
import { TabBar } from './components/TabBar';
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
        <span>v{version} · M2</span>
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
      <ConnectDialog />
      <HostKeyDialog />
      <KiDialog />
    </div>
  );
}
