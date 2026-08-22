import { useAppStore } from '../state/app-store';

const STATE_DOT: Record<string, string> = {
  connecting: 'bg-yellow-500',
  connected: 'bg-green-500',
  closed: 'bg-neutral-500',
  error: 'bg-red-500',
};

export function TabBar() {
  const tabs = useAppStore((s) => s.tabs);
  const activeId = useAppStore((s) => s.activeId);
  const setActive = useAppStore((s) => s.setActive);
  const closeTab = useAppStore((s) => s.closeTab);
  const openConnect = useAppStore((s) => s.openConnect);

  return (
    <div className="flex items-center gap-1 overflow-x-auto border-b border-neutral-800 bg-neutral-900 px-2 py-1">
      {tabs.map((t) => (
        <div
          key={t.id}
          className={`group flex cursor-pointer items-center gap-1.5 rounded px-2 py-1 text-xs ${
            t.id === activeId
              ? 'bg-neutral-700 text-neutral-100'
              : 'text-neutral-400 hover:bg-neutral-800'
          }`}
          onClick={() => setActive(t.id)}
        >
          <span className={`h-1.5 w-1.5 rounded-full ${STATE_DOT[t.state] ?? ''}`} />
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
      ))}
      <button
        className="rounded px-2 py-1 text-xs text-neutral-400 hover:bg-neutral-800 hover:text-neutral-100"
        onClick={openConnect}
        aria-label="new connection"
      >
        ＋ 新建
      </button>
    </div>
  );
}
