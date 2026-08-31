import { useAppStore } from '../state/app-store';
import { paneIds } from '../term/layout';
import { useT } from '../i18n';

/** pane 状态文案键：chrome.stateXxx */
const PANE_STATE_KEY = {
  connecting: 'chrome.stateConnecting',
  connected: 'chrome.stateConnected',
  reconnecting: 'chrome.stateReconnecting',
  closed: 'chrome.stateClosed',
  error: 'chrome.stateError',
} as const;

/**
 * 状态栏（12.2）：全部由现有 store 派生，无新增订阅/轮询。
 * 传输计数由 SFTP 面板现有 transfer_subscribe 顺带发布（面板从未打开时不显示该芯片）。
 * 无可靠延迟数据源，按规格不显示延迟。
 * 设置 ui.statusBar=false 隐藏。
 */
export function StatusBar() {
  const tabs = useAppStore((s) => s.tabs);
  const t = useT();
  const activeId = useAppStore((s) => s.activeId);
  const tunnels = useAppStore((s) => s.tunnels);
  const transferActive = useAppStore((s) => s.transferActive);
  const visible = useAppStore((s) => s.settings['ui.statusBar'] !== false);

  if (!visible) return null;

  const live = tabs.reduce(
    (n, t) =>
      n +
      paneIds(t.layout).filter((pid) => {
        const st = t.panes[pid]?.state;
        return st === 'connected' || st === 'connecting' || st === 'reconnecting';
      }).length,
    0,
  );
  const runningTunnels = tunnels.filter(
    (t) => t.status === 'listening' || t.status === 'starting' || t.status === 'reconnecting',
  ).length;
  const activeTab = tabs.find((t) => t.id === activeId);
  const activePane = activeTab?.panes[activeTab.activePaneId];

  return (
    <footer
      className="flex items-center gap-3 border-t border-neutral-800 px-3 py-0.5 text-xs text-neutral-500"
      role="status"
      aria-label={t('chrome.statusBarLabel')}
    >
      <span className="tabular-nums">{t('chrome.statusConnections', { count: live })}</span>
      <span className="tabular-nums">{t('chrome.statusTunnels', { count: runningTunnels })}</span>
      {transferActive !== null && (
        <span className="tabular-nums">
          {t('chrome.statusTransfers', { count: transferActive })}
        </span>
      )}
      <span className="ml-auto flex items-center gap-1.5 truncate">
        {activeTab && activePane && (
          <>
            <span
              className={`h-1.5 w-1.5 rounded-full ${
                activePane.state === 'connected'
                  ? 'bg-green-500'
                  : activePane.state === 'connecting' || activePane.state === 'reconnecting'
                    ? 'bg-yellow-500'
                    : 'bg-neutral-600'
              }`}
            />
            <span className="min-w-0 truncate">{activeTab.title}</span>
            <span className="text-neutral-600">
              {PANE_STATE_KEY[activePane.state as keyof typeof PANE_STATE_KEY]
                ? t(PANE_STATE_KEY[activePane.state as keyof typeof PANE_STATE_KEY])
                : activePane.state}
            </span>
          </>
        )}
      </span>
    </footer>
  );
}
