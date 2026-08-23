import { useAppStore } from '../state/app-store';

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
  const splitActive = useAppStore((s) => s.splitActive);
  const moveTab = useAppStore((s) => s.moveTab);
  const toggleSftp = useAppStore((s) => s.toggleSftp);
  const sftpOpen = useAppStore((s) => s.sftpOpen);
  const metricsOpen = useAppStore((s) => s.metricsOpen);
  const toggleMetrics = useAppStore((s) => s.toggleMetrics);

  const activeTab = tabs.find((t) => t.id === activeId);

  return (
    <div className="flex items-center gap-1 overflow-x-auto border-b border-neutral-800 bg-neutral-900 px-2 py-1">
      {tabs.map((t) => {
        const pane = t.panes[t.activePaneId];
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
          >
            <span className={`h-1.5 w-1.5 rounded-full ${STATE_DOT[pane?.state ?? ''] ?? ''}`} />
            <span className="max-w-40 truncate">{t.title}</span>
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
      {activeTab && (
        <span className="ml-1 flex gap-0.5 border-l border-neutral-700 pl-2">
          <button
            className="rounded px-1.5 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
            onClick={() => splitActive('row')}
            title="向右分屏（同主机新会话）"
          >
            ⬌
          </button>
          <button
            className="rounded px-1.5 py-0.5 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
            onClick={() => splitActive('col')}
            title="向下分屏（同主机新会话）"
          >
            ⬍
          </button>
          {activeTab.target.kind === 'session' && (
            <>
              <button
                className={`rounded px-1.5 py-0.5 text-xs ${
                  sftpOpen[activeTab.id]
                    ? 'bg-blue-800 text-neutral-100'
                    : 'text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100'
                }`}
                onClick={() => toggleSftp(activeTab.id)}
                title="SFTP 文件管理（仅档案会话）"
              >
                📂
              </button>
              <button
                className={`rounded px-1.5 py-0.5 text-xs ${
                  metricsOpen[activeTab.id]
                    ? 'bg-blue-800 text-neutral-100'
                    : 'text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100'
                }`}
                onClick={() => toggleMetrics(activeTab.id)}
                title="服务器监控（CPU/内存/网络/磁盘/进程）"
              >
                📈
              </button>
            </>
          )}
        </span>
      )}
      <button
        className="rounded px-2 py-1 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
        onClick={() => openConnect()}
        aria-label="new connection"
      >
        ＋ 新建
      </button>
    </div>
  );
}
