import { useEffect, useState } from 'react';
import { applyTerminalSettings, applyTheme } from './state/apply-settings';
import { keymapFromSettings, matchCombo } from './term/keymap';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { initIdPrefix } from './state/app-store';
import { invoke } from '@tauri-apps/api/core';
import { Sidebar } from './components/Sidebar';
import { TabBar } from './components/TabBar';
import { TunnelPanel } from './components/TunnelPanel';
import { CommandPalette } from './components/CommandPalette';
import { ConnectDialog } from './components/ConnectDialog';
import { HostKeyDialog } from './components/HostKeyDialog';
import { KiDialog } from './components/KiDialog';
import { SftpPanel } from './components/SftpPanel';
import { MetricsPanel } from './components/MetricsPanel';
import { SettingsDialog } from './components/SettingsDialog';
import { SplitTree } from './components/SplitTree';
import { useAppStore } from './state/app-store';
import { ConfirmDialog } from './components/ConfirmDialog';
import { Notices } from './components/Notices';
import { paneIds } from './term/layout';

export function App() {
  const [version, setVersion] = useState('…');
  const tabs = useAppStore((s) => s.tabs);
  const activeId = useAppStore((s) => s.activeId);
  const openConnect = useAppStore((s) => s.openConnect);
  const toggleSidebar = useAppStore((s) => s.toggleSidebar);
  const toggleTunnelPanel = useAppStore((s) => s.toggleTunnelPanel);
  const subscribeTunnels = useAppStore((s) => s.subscribeTunnels);
  const settingsOpen = useAppStore((s) => s.settingsOpen);
  const toggleSettings = useAppStore((s) => s.toggleSettings);
  const sftpOpen = useAppStore((s) => s.sftpOpen);
  const metricsOpen = useAppStore((s) => s.metricsOpen);
  const paletteOpen = useAppStore((s) => s.paletteOpen);
  const pendingDeleteSession = useAppStore((s) => s.pendingDeleteSession);
  const pendingCloseTab = useAppStore((s) => s.pendingCloseTab);
  const pendingCloseTabData = tabs.find((t) => t.id === pendingCloseTab) ?? null;
  // 关标签确认数据：pane 总数与即将断开的活跃连接数
  const pendingCloseIds = pendingCloseTabData ? paneIds(pendingCloseTabData.layout) : [];
  const pendingCloseLive = pendingCloseIds.filter((pid) => {
    const st = pendingCloseTabData?.panes[pid]?.state;
    return st === 'connected' || st === 'connecting' || st === 'reconnecting';
  }).length;

  useEffect(() => {
    subscribeTunnels();
  }, [subscribeTunnels]);

  // 全局快捷键（注册表驱动，M5）：xterm 焦点下 window keydown 仍可收到
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const s = useAppStore.getState();
      const bindings = keymapFromSettings(s.settings);
      const hit = (id: string) => bindings[id] && matchCombo(e, bindings[id]);
      const active = s.tabs.find((t) => t.id === s.activeId);
      if (hit('palette')) s.togglePalette();
      else if (hit('newTab')) s.openConnect();
      else if (hit('closeTab') && s.activeId) s.closeTab(s.activeId);
      else if (hit('nextTab') || hit('prevTab')) {
        if (s.tabs.length > 1 && s.activeId) {
          const i = s.tabs.findIndex((t) => t.id === s.activeId);
          const d = hit('nextTab') ? 1 : -1;
          s.setActive(s.tabs[(i + d + s.tabs.length) % s.tabs.length].id);
        }
      } else if (hit('sftp') && active?.target.kind === 'session') s.toggleSftp(active.id);
      else if (hit('metrics') && active?.target.kind === 'session') s.toggleMetrics(active.id);
      else if (hit('tunnels')) s.toggleTunnelPanel();
      else if (hit('settings')) s.toggleSettings();
      else if (hit('splitRow')) s.splitActive('row');
      else if (hit('splitCol')) s.splitActive('col');
      else return;
      e.preventDefault();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  useEffect(() => {
    // 纯浏览器（vite 诊断）下 invoke 不可用
    invoke<string>('app_version')
      .then(setVersion)
      .catch(() => setVersion('dev'));
    // 标签分离（M5）：?detach=<sessionId> 的独立窗口启动即连接该会话
    initIdPrefix(getCurrentWindow().label);
    const detach = new URLSearchParams(window.location.search).get('detach');
    if (detach) {
      const s = useAppStore.getState();
      s.loadSessions()
        .then(() => {
          const rec = useAppStore.getState().sessions.find((r) => r.id === detach);
          if (rec) useAppStore.getState().connectBySession(rec.id, rec.name);
        })
        .catch(() => undefined);
    }
  }, []);

  // 设置：启动拉取一次；变更即应用（主题/终端字体）；system 主题跟随 OS 切换
  const settings = useAppStore((s) => s.settings);
  const settingsLoaded = useAppStore((s) => s.settingsLoaded);
  const loadSettings = useAppStore((s) => s.loadSettings);
  useEffect(() => {
    if (!settingsLoaded) {
      loadSettings().catch(() => undefined);
      return;
    }
    applyTheme(settings);
    applyTerminalSettings(settings);
  }, [settings, settingsLoaded, loadSettings]);
  useEffect(() => {
    const mq = window.matchMedia('(prefers-color-scheme: dark)');
    const onChange = () => {
      if (useAppStore.getState().settings['theme'] === 'system') {
        applyTheme(useAppStore.getState().settings);
      }
    };
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
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
          <button
            className="rounded px-1 hover:bg-neutral-800"
            onClick={toggleSettings}
            title="设置（Ctrl+,）"
          >
            ⚙
          </button>
          <span>v{version}</span>
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
      {activeId && sftpOpen[activeId] && <SftpPanel tabId={activeId} />}
      {activeId && metricsOpen[activeId] && <MetricsPanel tabId={activeId} />}
      <TunnelPanel />
      <ConnectDialog />
      <HostKeyDialog />
      <KiDialog />
      {paletteOpen && <CommandPalette />}
      {settingsOpen && <SettingsDialog />}
      {/* 批次一 7.1：删除服务器确认（级联清凭据，不可撤销；默认焦点在取消） */}
      {pendingDeleteSession && (
        <ConfirmDialog
          title={`删除服务器“${pendingDeleteSession.name}”？`}
          confirmLabel="删除服务器"
          onCancel={() => useAppStore.getState().cancelDeleteSession()}
          onConfirm={() => void useAppStore.getState().confirmDeleteSession()}
        >
          <p className="mb-1 text-neutral-300">
            主机：{pendingDeleteSession.username}@{pendingDeleteSession.host}:
            {pendingDeleteSession.port}
          </p>
          <p className="mb-1">
            分组：{pendingDeleteSession.groupPath.replace(/\//g, ' / ') || '未分组'}
          </p>
          <p className="mb-1">删除后，该服务器保存的密码或凭据也会一并删除。</p>
          <p className="text-red-300">此操作无法撤销。</p>
        </ConfirmDialog>
      )}
      {/* 批次一 7.6：关闭有活跃连接的标签前确认 */}
      {pendingCloseTabData && (
        <ConfirmDialog
          title={`关闭标签“${pendingCloseTabData.title}”？`}
          confirmLabel="关闭标签"
          onCancel={() => useAppStore.getState().cancelCloseTab()}
          onConfirm={() => useAppStore.getState().confirmCloseTab()}
        >
          <p className="mb-1">
            该标签包含 {pendingCloseIds.length} 个窗格，其中 {pendingCloseLive} 个连接仍活跃。
          </p>
          <p className="text-red-300">关闭后，{pendingCloseLive} 个活跃连接将立即断开。</p>
        </ConfirmDialog>
      )}
      <Notices />
    </div>
  );
}
