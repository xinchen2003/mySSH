import { useEffect } from 'react';
import { isLocalTarget, useAppStore } from '../state/app-store';
import { initBroadcastReceiver } from '../term/broadcast';
import { keymapFromSettings } from '../term/keymap';
import { useT } from '../i18n';

/**
 * 右上角统一工具条（UX 批次 · 条目 8/13）：收纳原散落在窗口头部与 TabBar 的
 * 功能图标，统一尺寸/hover/激活态/tooltip（含快捷键提示）；活跃面板对应图标高亮。
 * 顺序约定：面板（SFTP/监控/隧道/广播/传输中心）→ 设置。
 * 分屏入口在右键菜单与快捷键（keymap splitRow/splitCol），工具条不再直挂。
 *
 * SFTP/监控/隧道/传输中心四个按钮统一走底部 dock（app-store dockTab/toggleDock）。
 */
export function AppToolbar() {
  const tabs = useAppStore((s) => s.tabs);
  const t = useT();
  const activeId = useAppStore((s) => s.activeId);
  const dockTab = useAppStore((s) => s.dockTab);
  const toggleDock = useAppStore((s) => s.toggleDock);
  const settingsOpen = useAppStore((s) => s.settingsOpen);
  const toggleSettings = useAppStore((s) => s.toggleSettings);
  const broadcastEnabled = useAppStore((s) => s.broadcastEnabled);
  const toggleBroadcast = useAppStore((s) => s.toggleBroadcast);
  const transferActive = useAppStore((s) => s.transferActive);
  const settings = useAppStore((s) => s.settings);

  // 广播接收端注册（幂等；detached 子窗口各跑一份 App 同样生效）
  useEffect(() => {
    void initBroadcastReceiver();
  }, []);

  const activeTab = tabs.find((t) => t.id === activeId);
  const isSession = activeTab?.target.kind === 'session';
  // 本地会话（kind==='local'）：SFTP/监控等 SSH 专属功能禁用；分屏/分离窗口/广播天然可用
  const isRemote = isSession && !(activeTab && isLocalTarget(activeTab.target));

  // 快捷键提示：跟随设置中的方案/自定义绑定
  const bindings = keymapFromSettings(settings);
  const kb = (id: string) => (bindings[id] ? t('chrome.kbHint', { key: bindings[id] }) : '');

  return (
    <div className="flex items-center gap-0.5" role="toolbar" aria-label={t('chrome.toolbarLabel')}>
      <ToolBtn
        label={t('chrome.sftpFiles')}
        tooltip={t('chrome.tipSftp', { kb: kb('sftp') })}
        active={dockTab === 'sftp'}
        disabled={!isRemote}
        onClick={() => toggleDock('sftp')}
      >
        📂
      </ToolBtn>
      <ToolBtn
        label={t('chrome.metricsLabel')}
        tooltip={t('chrome.tipMetrics', { kb: kb('metrics') })}
        active={dockTab === 'metrics'}
        disabled={!isRemote}
        onClick={() => toggleDock('metrics')}
      >
        📈
      </ToolBtn>
      <ToolBtn
        label={t('chrome.tunnelsLabel')}
        tooltip={t('chrome.tipTunnels', { kb: kb('tunnels') })}
        active={dockTab === 'tunnel'}
        onClick={() => toggleDock('tunnel')}
      >
        ⇄
      </ToolBtn>
      <ToolBtn
        label={t('chrome.broadcastLabel')}
        tooltip={t('chrome.tipBroadcast')}
        active={broadcastEnabled}
        onClick={toggleBroadcast}
      >
        📡
      </ToolBtn>
      <ToolBtn
        label={t('chrome.transferLabel')}
        tooltip={t('chrome.tipTransfer', {
          active: transferActive ? t('chrome.transferActive', { count: transferActive }) : '',
        })}
        active={dockTab === 'transfer'}
        // dock 未停在传输页签时用角标报告进行中数量；打开后列表自带状态
        badge={dockTab !== 'transfer' && transferActive ? transferActive : undefined}
        onClick={() => toggleDock('transfer')}
      >
        ⇅
      </ToolBtn>

      <Divider />

      <ToolBtn
        label={t('chrome.settingsLabel')}
        tooltip={t('chrome.tipSettings', { kb: kb('settings') })}
        active={settingsOpen}
        onClick={toggleSettings}
      >
        ⚙
      </ToolBtn>
    </div>
  );
}

/** 统一风格的工具条图标按钮：固定尺寸、hover、激活高亮、禁用降透明度 */
function ToolBtn({
  label,
  tooltip,
  active,
  disabled,
  onClick,
  badge,
  children,
}: {
  label: string;
  tooltip: string;
  active?: boolean;
  disabled?: boolean;
  onClick?: () => void;
  /** 进行中数量角标（>0 才显示） */
  badge?: number;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      className={`relative flex h-6 w-6 items-center justify-center rounded text-sm leading-none ${
        active
          ? 'bg-blue-800 text-neutral-100'
          : 'text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100'
      } disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-transparent disabled:hover:text-neutral-400`}
      disabled={disabled}
      onClick={onClick}
      title={tooltip}
      aria-label={label}
    >
      {children}
      {badge !== undefined && badge > 0 && (
        <span className="absolute -right-1 -top-1 flex h-3.5 min-w-3.5 animate-pulse items-center justify-center rounded-full bg-blue-600 px-0.5 text-[9px] font-medium leading-none text-white">
          {badge > 99 ? '99+' : badge}
        </span>
      )}
    </button>
  );
}

function Divider() {
  return <span className="mx-1 h-4 w-px bg-neutral-700" />;
}
