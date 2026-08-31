import { isLocalTarget, useAppStore, type DockTab } from '../state/app-store';
import { keymapFromSettings } from '../term/keymap';
import { usePanelHeight } from './panel-height';
import { SftpPanel } from './SftpPanel';
import { MetricsPanel } from './MetricsPanel';
import { TunnelPanel } from './TunnelPanel';
import { TransferCenter } from './TransferCenter';
import { useT, type MsgKey } from '../i18n';

/** dock 页签定义（顺序即 tab 栏顺序）；labelKey 为 chrome 组文案键 */
const DOCK_TABS: { id: DockTab; labelKey: MsgKey; keyId?: string }[] = [
  { id: 'sftp', labelKey: 'chrome.dockSftp', keyId: 'sftp' },
  { id: 'metrics', labelKey: 'chrome.dockMetrics', keyId: 'metrics' },
  { id: 'tunnel', labelKey: 'chrome.dockTunnel', keyId: 'tunnels' },
  { id: 'transfer', labelKey: 'chrome.transferLabel' },
];

/**
 * 底部工具 dock（Chrome DevTools 风格）：顶部 tab 栏（SFTP/监控/隧道/传输中心 + 右侧关闭），
 * 下方内容区按 app-store 的 dockTab 渲染对应面板；顶缘拖拽调高度（usePanelHeight）。
 * dockTab 为窗口级单值，四面板天然互斥；开关语义统一走 openDock/closeDock/toggleDock。
 *
 * SFTP/监控是 SSH 专属能力：活跃终端为本地会话时 tab 禁用（降透明度 + tooltip），
 * 已打开时内容区显示占位提示而非面板。
 */
export function BottomDock() {
  const dockTab = useAppStore((s) => s.dockTab);
  const t = useT();
  const toggleDock = useAppStore((s) => s.toggleDock);
  const closeDock = useAppStore((s) => s.closeDock);
  const activeId = useAppStore((s) => s.activeId);
  const tabs = useAppStore((s) => s.tabs);
  const settings = useAppStore((s) => s.settings);
  const { height, handle } = usePanelHeight('ui.dockHeight', 288);

  if (!dockTab || !activeId) return null;

  const activeTab = tabs.find((t) => t.id === activeId);
  // 本地会话：SFTP/监控 tab 禁用；dock 已停在此页签时内容区给占位提示
  const isLocal = activeTab ? isLocalTarget(activeTab.target) : false;
  const sshOnly = dockTab === 'sftp' || dockTab === 'metrics';

  const bindings = keymapFromSettings(settings);
  const kb = (id?: string) => (id && bindings[id] ? t('chrome.kbHint', { key: bindings[id] }) : '');

  return (
    <div
      className="flex shrink-0 flex-col border-t border-neutral-700 bg-neutral-900"
      style={{ height }}
    >
      {handle}
      {/* tab 栏：页签 + 右侧关闭 */}
      <div
        className="flex shrink-0 items-center gap-0.5 border-b border-neutral-800 px-2"
        role="tablist"
        aria-label={t('chrome.dockTablistAria')}
      >
        {DOCK_TABS.map((tab) => {
          const label = t(tab.labelKey);
          const disabled = (tab.id === 'sftp' || tab.id === 'metrics') && isLocal;
          const active = dockTab === tab.id;
          return (
            <button
              key={tab.id}
              type="button"
              role="tab"
              aria-selected={active}
              aria-label={t('chrome.dockTabAria', { label })}
              disabled={disabled}
              title={
                disabled
                  ? t('chrome.dockTabDisabledTip', { label })
                  : t('chrome.dockTabTip', { label, kb: kb(tab.keyId) })
              }
              className={`mt-1 rounded-t px-3 py-1 text-xs focus-visible:ring-1 focus-visible:ring-neutral-500 ${
                active
                  ? 'bg-neutral-800 font-medium text-neutral-100'
                  : 'text-neutral-400 hover:bg-neutral-800/60 hover:text-neutral-200'
              } disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent`}
              onClick={() => toggleDock(tab.id)}
            >
              {label}
            </button>
          );
        })}
        <span className="flex-1" />
        <button
          type="button"
          className="rounded px-1.5 py-0.5 text-neutral-500 hover:bg-neutral-800 hover:text-neutral-200 focus-visible:ring-1 focus-visible:ring-neutral-500"
          onClick={closeDock}
          aria-label={t('chrome.dockClose')}
          title={t('chrome.dockClose')}
        >
          ✕
        </button>
      </div>
      {/* 内容区 */}
      <div className="min-h-0 flex-1">
        {sshOnly && isLocal ? (
          <div className="px-3 py-2 text-xs text-neutral-500">
            {dockTab === 'sftp' ? t('chrome.dockLocalSftp') : t('chrome.dockLocalMetrics')}
          </div>
        ) : dockTab === 'sftp' ? (
          <SftpPanel tabId={activeId} />
        ) : dockTab === 'metrics' ? (
          <MetricsPanel tabId={activeId} />
        ) : dockTab === 'tunnel' ? (
          <TunnelPanel />
        ) : (
          <TransferCenter />
        )}
      </div>
    </div>
  );
}
