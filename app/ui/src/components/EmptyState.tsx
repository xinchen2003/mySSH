import { useAppStore } from '../state/app-store';
import { GROUP_KEYS, readStringList } from '../state/groups';
import { KEY_ACTIONS, keymapFromSettings } from '../term/keymap';
import { useT } from '../i18n';

/** 空态展示的常用快捷键（取生效绑定） */
const EMPTY_KEY_IDS = ['palette', 'newTab', 'closeTab', 'search', 'sftp'] as const;

/**
 * 空态（12.5）：无打开标签时的起始页。
 * 全部数据来自已加载的 sessions/settings，无任何新增订阅或轮询。
 */
export function EmptyState() {
  const openConnect = useAppStore((s) => s.openConnect);
  const t = useT();
  const toggleQuickConnect = useAppStore((s) => s.toggleQuickConnect);
  const togglePalette = useAppStore((s) => s.togglePalette);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const sessions = useAppStore((s) => s.sessions);
  const settings = useAppStore((s) => s.settings);

  const recent = readStringList(settings[GROUP_KEYS.recent])
    .slice(0, 3)
    .map((id) => sessions.find((s) => s.id === id))
    .filter((s) => s !== undefined);
  const bindings = keymapFromSettings(settings);
  const keyRows = EMPTY_KEY_IDS.map((id) => ({
    label: KEY_ACTIONS.find((a) => a.id === id)?.label ?? id,
    combo: bindings[id] ?? '',
  }));

  const btnCls =
    'rounded border border-neutral-700 px-4 py-2 text-sm text-neutral-300 hover:bg-neutral-800 hover:text-neutral-100';

  return (
    <div className="flex h-full items-center justify-center" data-testid="empty-state">
      <div className="w-96 max-w-[90vw]">
        <p className="mb-4 text-center text-sm text-neutral-500">{t('chrome.emptyTitle')}</p>
        <div className="flex justify-center gap-2">
          <button className={btnCls} onClick={() => openConnect()}>
            {t('chrome.emptyNewServer')}
          </button>
          <button className={btnCls} onClick={toggleQuickConnect}>
            {t('chrome.emptyQuickConnect')}
          </button>
          <button className={btnCls} onClick={togglePalette}>
            {t('chrome.emptyPalette')}
          </button>
        </div>
        {recent.length > 0 && (
          <div className="mt-6">
            <p className="mb-1.5 text-xs text-neutral-600">{t('chrome.emptyRecent')}</p>
            <ul className="space-y-1">
              {recent.map((s) => (
                <li key={s.id}>
                  <button
                    className="flex w-full items-center justify-between rounded px-3 py-1.5 text-left text-sm text-neutral-300 hover:bg-neutral-800"
                    onClick={() => connectBySession(s.id, s.name)}
                  >
                    <span className="min-w-0 truncate">{s.name}</span>
                    <span className="shrink-0 text-xs text-neutral-600">
                      {s.username}@{s.host}:{s.port}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        )}
        <div className="mt-6">
          <p className="mb-1.5 text-xs text-neutral-600">{t('chrome.emptyShortcuts')}</p>
          <ul className="space-y-0.5 text-xs text-neutral-500">
            {keyRows.map((r) => (
              <li key={r.label} className="flex justify-between">
                <span>{r.label}</span>
                <span className="text-neutral-600">{r.combo}</span>
              </li>
            ))}
          </ul>
        </div>
      </div>
    </div>
  );
}
