import { useState } from 'react';
import { useAppStore, type Tab } from '../state/app-store';
import { ContextMenu, type MenuItem } from './ContextMenu';

const STATE_DOT: Record<string, string> = {
  connecting: 'bg-yellow-500',
  connected: 'bg-green-500',
  reconnecting: 'bg-orange-500',
  closed: 'bg-neutral-500',
  error: 'bg-red-500',
};

export function TabBar() {
  const tabs = useAppStore((s) => s.tabs);
  const activeId = useAppStore((s) => s.activeId);
  const setActive = useAppStore((s) => s.setActive);
  const closeTab = useAppStore((s) => s.closeTab);
  const openConnect = useAppStore((s) => s.openConnect);
  const moveTab = useAppStore((s) => s.moveTab);
  const bellTabs = useAppStore((s) => s.bellTabs);
  const sessions = useAppStore((s) => s.sessions);

  /** 标签右键菜单（批次四 10.3） */
  const [menu, setMenu] = useState<{ x: number; y: number; tab: Tab } | null>(null);

  const tabMenuItems = (t: Tab): MenuItem[] => {
    const s = useAppStore.getState();
    const idx = s.tabs.findIndex((x) => x.id === t.id);
    const isSession = t.target.kind === 'session';
    const sessionId = t.target.kind === 'session' ? t.target.sessionId : null;
    return [
      { label: '重新连接', onSelect: () => s.reconnectTab(t.id) },
      { label: '断开', onSelect: () => s.disconnectTab(t.id) },
      'separator',
      {
        label: '打开 SFTP',
        disabled: !isSession,
        onSelect: () => {
          s.setActive(t.id);
          if (!s.sftpOpen[t.id]) s.toggleSftp(t.id);
        },
      },
      {
        label: '打开监控',
        disabled: !isSession,
        onSelect: () => {
          s.setActive(t.id);
          if (!s.metricsOpen[t.id]) s.toggleMetrics(t.id);
        },
      },
      {
        label: '管理隧道',
        onSelect: () => {
          if (!s.tunnelPanelOpen) s.toggleTunnelPanel();
        },
      },
      'separator',
      {
        label: '向右分屏',
        onSelect: () => {
          s.setActive(t.id);
          s.splitActive('row');
        },
      },
      {
        label: '向下分屏',
        onSelect: () => {
          s.setActive(t.id);
          s.splitActive('col');
        },
      },
      {
        label: '分离窗口',
        disabled: !isSession,
        onSelect: () => {
          if (sessionId) s.connectInNewWindow(sessionId, t.title);
        },
      },
      'separator',
      { label: '关闭', onSelect: () => s.closeTab(t.id) },
      {
        label: '关闭其他',
        disabled: s.tabs.length < 2,
        onSelect: () => s.closeOtherTabs(t.id),
      },
      {
        label: '关闭右侧',
        disabled: idx < 0 || idx >= s.tabs.length - 1,
        onSelect: () => s.closeTabsToRight(t.id),
      },
      { label: '关闭全部', disabled: s.tabs.length < 2, onSelect: () => s.closeAllTabs() },
    ];
  };

  return (
    <div className="flex items-center gap-1 overflow-x-auto border-b border-neutral-800 bg-neutral-900 px-2 py-1">
      {tabs.map((t) => {
        const pane = t.panes[t.activePaneId];
        // 标签颜色点：session 标签从 sessions 列表取 record.color；本地终端等无关联会话不渲染
        const tabSessionId = t.target.kind === 'session' ? t.target.sessionId : null;
        const sessionColor = tabSessionId
          ? (sessions.find((r) => r.id === tabSessionId)?.color ?? null)
          : null;
        return (
          <div
            key={t.id}
            draggable
            onDragStart={(e) => e.dataTransfer.setData('text/myssh-tab', t.id)}
            onDragOver={(e) => e.preventDefault()}
            onDrop={(e) => {
              e.preventDefault();
              const dragId = e.dataTransfer.getData('text/myssh-tab');
              if (dragId) moveTab(dragId, t.id);
            }}
            className={`group flex cursor-pointer items-center gap-1.5 rounded px-2 py-1 text-xs ${
              t.id === activeId
                ? 'bg-neutral-700 text-neutral-100'
                : 'text-neutral-400 hover:bg-neutral-800'
            }`}
            onClick={() => setActive(t.id)}
            onAuxClick={(e) => {
              // 中键关闭（批次四 10.3）；走 closeTab 确认守卫
              if (e.button === 1) {
                e.preventDefault();
                closeTab(t.id);
              }
            }}
            onContextMenu={(e) => {
              e.preventDefault();
              setMenu({ x: e.clientX, y: e.clientY, tab: t });
            }}
          >
            <span className={`h-1.5 w-1.5 rounded-full ${STATE_DOT[pane?.state ?? ''] ?? ''}`} />
            {sessionColor && (
              <span
                className="h-2 w-2 shrink-0 rounded-full"
                style={{ backgroundColor: sessionColor }}
              />
            )}
            <span className="max-w-40 truncate">{t.title}</span>
            {/* 12.6：bell 待读标记（非活跃标签收到 BEL；激活即清，静态点无动画） */}
            {bellTabs.includes(t.id) && (
              <span className="h-1.5 w-1.5 rounded-full bg-blue-400" title="终端响铃" />
            )}
            <button
              className="ml-1 hidden text-neutral-500 hover:text-neutral-200 group-hover:inline"
              onClick={(e) => {
                e.stopPropagation();
                closeTab(t.id);
              }}
              aria-label="close tab"
            >
              ×
            </button>
          </div>
        );
      })}
      {/* 功能图标统一收进 AppToolbar（UX 批次 · 条目 8）；此处只保留标签与新建按钮 */}
      <button
        className="rounded px-2 py-1 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
        onClick={() => openConnect()}
        aria-label="new connection"
      >
        新建
      </button>
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={tabMenuItems(menu.tab)}
          onClose={() => setMenu(null)}
        />
      )}
    </div>
  );
}
