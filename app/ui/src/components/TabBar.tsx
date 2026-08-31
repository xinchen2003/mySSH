import { useState } from 'react';
import { isLocalTarget, useAppStore, type Tab } from '../state/app-store';
import { ContextMenu, type MenuItem } from './ContextMenu';
import { useT } from '../i18n';

const STATE_DOT: Record<string, string> = {
  connecting: 'bg-yellow-500',
  connected: 'bg-green-500',
  reconnecting: 'bg-orange-500',
  closed: 'bg-neutral-500',
  error: 'bg-red-500',
};

export function TabBar() {
  const tabs = useAppStore((s) => s.tabs);
  const tr = useT();
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
    // 本地会话禁用 SSH 专属入口（SFTP/监控/隧道）；分屏/分离窗口可用
    const isRemote = isSession && !isLocalTarget(t.target);
    return [
      { label: tr('chrome.menuReconnect'), icon: '▶', onSelect: () => s.reconnectTab(t.id) },
      { label: tr('chrome.menuDisconnect'), icon: '⏻', onSelect: () => s.disconnectTab(t.id) },
      'separator',
      {
        label: tr('chrome.menuOpenSftp'),
        icon: '⇅',
        disabled: !isRemote,
        onSelect: () => {
          s.setActive(t.id);
          s.openDock('sftp');
        },
      },
      {
        label: tr('chrome.menuOpenMetrics'),
        icon: '∿',
        disabled: !isRemote,
        onSelect: () => {
          s.setActive(t.id);
          s.openDock('metrics');
        },
      },
      {
        label: tr('chrome.menuTunnels'),
        icon: '⇄',
        disabled: isSession && !isRemote,
        onSelect: () => {
          s.openDock('tunnel');
        },
      },
      'separator',
      {
        label: tr('chrome.menuSplitRight'),
        onSelect: () => {
          s.setActive(t.id);
          s.splitActive('row');
        },
      },
      {
        label: tr('chrome.menuSplitDown'),
        onSelect: () => {
          s.setActive(t.id);
          s.splitActive('col');
        },
      },
      {
        label: tr('chrome.menuDetach'),
        icon: '⬈',
        disabled: !isSession,
        onSelect: () => {
          if (sessionId) s.connectInNewWindow(sessionId, t.title);
        },
      },
      'separator',
      { label: tr('chrome.menuClose'), icon: '✕', onSelect: () => s.closeTab(t.id) },
      {
        label: tr('chrome.menuCloseOthers'),
        icon: '✕',
        disabled: s.tabs.length < 2,
        onSelect: () => s.closeOtherTabs(t.id),
      },
      {
        label: tr('chrome.menuCloseRight'),
        icon: '✕',
        disabled: idx < 0 || idx >= s.tabs.length - 1,
        onSelect: () => s.closeTabsToRight(t.id),
      },
      {
        label: tr('chrome.menuCloseAll'),
        icon: '✕',
        disabled: s.tabs.length < 2,
        onSelect: () => s.closeAllTabs(),
      },
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
            // 标签颜色：整 tab 附着半透明底色（活跃 40% / 非活跃 25% alpha），比色点显眼
            style={
              sessionColor
                ? { backgroundColor: `${sessionColor}${t.id === activeId ? '66' : '40'}` }
                : undefined
            }
            className={`group flex cursor-pointer items-center gap-1.5 rounded px-2 py-1 text-xs focus-visible:ring-1 focus-visible:ring-neutral-500 ${
              t.id === activeId
                ? `${sessionColor ? '' : 'bg-neutral-700 '}text-neutral-100`
                : 'text-neutral-400 hover:bg-neutral-800'
            }`}
            role="tab"
            tabIndex={0}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                setActive(t.id);
              }
            }}
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
            <span className="max-w-40 truncate">{t.title}</span>
            {/* 12.6：bell 待读标记（非活跃标签收到 BEL；激活即清，静态点无动画） */}
            {bellTabs.includes(t.id) && (
              <span
                className="h-1.5 w-1.5 rounded-full bg-blue-400"
                title={tr('chrome.bellTitle')}
              />
            )}
            <button
              className="ml-1 hidden group-focus-within:inline text-neutral-500 hover:text-neutral-200 group-hover:inline"
              onClick={(e) => {
                e.stopPropagation();
                closeTab(t.id);
              }}
              aria-label={tr('chrome.closeTabAria')}
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
        aria-label={tr('chrome.newConnection')}
      >
        {tr('chrome.newConnection')}
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
