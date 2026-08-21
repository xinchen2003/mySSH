import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { TerminalView } from './terminal/TerminalView';

export function App() {
  const [version, setVersion] = useState('…');

  useEffect(() => {
    // 纯浏览器（vite 诊断）下 invoke 不可用
    invoke<string>('app_version')
      .then(setVersion)
      .catch(() => setVersion('dev'));
  }, []);

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center justify-between border-b border-neutral-800 px-3 py-1.5 text-xs text-neutral-400">
        <span className="font-semibold text-neutral-200">mySSH</span>
        <span>v{version} · M0</span>
      </header>
      <main className="min-h-0 flex-1">
        <TerminalView />
      </main>
    </div>
  );
}
