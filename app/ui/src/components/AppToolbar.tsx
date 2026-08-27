import { useEffect, useRef } from 'react';
import { isLocalTarget, useAppStore } from '../state/app-store';
import { initBroadcastReceiver } from '../term/broadcast';
import { useTransferStore } from '../state/transfer-store';
import { keymapFromSettings } from '../term/keymap';
import { TunnelPopover } from './TunnelPopover';

/**
 * 右上角统一工具条（UX 批次 · 条目 8/13）：收纳原散落在窗口头部与 TabBar 的
 * 功能图标，统一尺寸/hover/激活态/tooltip（含快捷键提示）；活跃面板对应图标高亮。
 * 顺序约定：分屏 → 面板（SFTP/监控/隧道/广播/传输中心）→ 设置 → ⧉ 新窗口恒居最右（条目 13）。
 *
 * 隧道按钮自带锚定弹层 TunnelPopover（条目 7，复用 store tunnelPanelOpen）。
 *
 * 依赖 store 新增状态（由 Main 装配时对齐，见 local://intg-chrome.md）：
 * - broadcastEnabled: boolean / toggleBroadcast(): void
 * 传输中心开关走 transfer-store（useTransferStore: open/toggleOpen，归 SftpBatch）。
 */
export function AppToolbar() {
  const tabs = useAppStore((s) => s.tabs);
  const activeId = useAppStore((s) => s.activeId);
  const splitActive = useAppStore((s) => s.splitActive);
  const sftpOpen = useAppStore((s) => s.sftpOpen);
  const toggleSftp = useAppStore((s) => s.toggleSftp);
  const metricsOpen = useAppStore((s) => s.metricsOpen);
  const toggleMetrics = useAppStore((s) => s.toggleMetrics);
  const tunnelPanelOpen = useAppStore((s) => s.tunnelPanelOpen);
  const toggleTunnelPanel = useAppStore((s) => s.toggleTunnelPanel);
  const settingsOpen = useAppStore((s) => s.settingsOpen);
  const toggleSettings = useAppStore((s) => s.toggleSettings);
  const broadcastEnabled = useAppStore((s) => s.broadcastEnabled);
  const toggleBroadcast = useAppStore((s) => s.toggleBroadcast);
  const transferCenterOpen = useTransferStore((s) => s.open);
  const toggleTransferCenter = useTransferStore((s) => s.toggleOpen);
  const transferActive = useAppStore((s) => s.transferActive);
  const settings = useAppStore((s) => s.settings);

  const tunnelAnchorRef = useRef<HTMLDivElement>(null);
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
  const kb = (id: string) => (bindings[id] ? `（${bindings[id]}）` : '');

  return (
    <div className="flex items-center gap-0.5" role="toolbar" aria-label="功能工具条">
      <ToolBtn
        label="向右分屏"
        tooltip={`向右分屏（同主机新会话）${kb('splitRow')}`}
        disabled={!activeTab}
        onClick={() => splitActive('row')}
      >
        ⬌
      </ToolBtn>
      <ToolBtn
        label="向下分屏"
        tooltip={`向下分屏（同主机新会话）${kb('splitCol')}`}
        disabled={!activeTab}
        onClick={() => splitActive('col')}
      >
        ⬍
      </ToolBtn>

      <Divider />

      <ToolBtn
        label="SFTP 文件管理"
        tooltip={`SFTP 文件管理（仅远程会话）${kb('sftp')}`}
        active={!!activeTab && !!sftpOpen[activeTab.id]}
        disabled={!isRemote}
        onClick={() => activeTab && toggleSftp(activeTab.id)}
      >
        📂
      </ToolBtn>
      <ToolBtn
        label="服务器监控"
        tooltip={`服务器监控（CPU/内存/网络/磁盘/进程）${kb('metrics')}`}
        active={!!activeTab && !!metricsOpen[activeTab.id]}
        disabled={!isRemote}
        onClick={() => activeTab && toggleMetrics(activeTab.id)}
      >
        📈
      </ToolBtn>
      <div ref={tunnelAnchorRef} className="relative">
        <ToolBtn
          label="隧道管理"
          tooltip={`隧道管理${kb('tunnels')}`}
          active={tunnelPanelOpen}
          onClick={toggleTunnelPanel}
        >
          ⇄
        </ToolBtn>
        <TunnelPopover anchorRef={tunnelAnchorRef} />
      </div>
      <ToolBtn
        label="广播输入"
        tooltip="广播输入到所有已连接会话"
        active={broadcastEnabled}
        onClick={toggleBroadcast}
      >
        📡
      </ToolBtn>
      <ToolBtn
        label="传输中心"
        tooltip={`传输中心（SFTP 上传/下载队列）${transferActive ? `——进行中 ${transferActive} 项` : ''}`}
        active={transferCenterOpen}
        // 抽屉收起时用角标报告进行中数量；展开后列表自带状态
        badge={!transferCenterOpen && transferActive ? transferActive : undefined}
        onClick={toggleTransferCenter}
      >
        ⇅
      </ToolBtn>

      <Divider />

      <ToolBtn
        label="设置"
        tooltip={`设置${kb('settings')}`}
        active={settingsOpen}
        onClick={toggleSettings}
      >
        ⚙
      </ToolBtn>

      <Divider />

      {/* 条目 13：会话拓展方式「⧉ 新窗口」恒居所有菜单按钮最右 */}
      <ToolBtn
        label="在新窗口打开此会话"
        tooltip="在新窗口打开此会话（标签分离）"
        disabled={!isSession}
        onClick={() => {
          const t = activeTab?.target;
          if (!activeTab || t?.kind !== 'session') return;
          useAppStore.getState().connectInNewWindow(t.sessionId, activeTab.title);
        }}
      >
        ⧉
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
