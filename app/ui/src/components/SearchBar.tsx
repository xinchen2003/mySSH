import { useEffect, useRef, useState } from 'react';
import { useT } from '../i18n';

export interface SearchOptions {
  caseSensitive: boolean;
  wholeWord: boolean;
}

export interface SearchResults {
  /** 当前命中序号（0 起；-1 = 无当前命中） */
  index: number;
  count: number;
}

/**
 * 终端搜索浮条（Ctrl+Shift+F 唤起）：输入即增量查找，
 * Enter 下一个 / Shift+Enter 上一个 / Esc 关闭。
 * 12.7：匹配计数（依赖 SearchAddon decorations 追踪）、区分大小写、全词匹配。
 */
export function SearchBar({
  onFind,
  onClose,
  results,
}: {
  onFind: (query: string, dir: 'next' | 'prev', opts: SearchOptions) => void;
  onClose: () => void;
  results: SearchResults | null;
}) {
  const t = useT();
  const inputRef = useRef<HTMLInputElement>(null);
  const [opts, setOpts] = useState<SearchOptions>({ caseSensitive: false, wholeWord: false });
  useEffect(() => inputRef.current?.focus(), []);

  const find = (dir: 'next' | 'prev', o: SearchOptions = opts) =>
    onFind(inputRef.current?.value ?? '', dir, o);

  const toggleBtn = (on: boolean) =>
    `rounded px-1 text-xs ${on ? 'bg-blue-700 text-white' : 'text-neutral-400 hover:text-neutral-100'}`;

  return (
    <div className="absolute top-1 right-3 z-10 flex items-center gap-1 rounded border border-neutral-700 bg-neutral-900/95 px-2 py-1 shadow-lg">
      <input
        ref={inputRef}
        className="w-48 rounded bg-neutral-800 px-2 py-0.5 text-xs text-neutral-200 outline-none focus-visible:ring-1 focus-visible:ring-neutral-500"
        placeholder={t('panels.searchScrollback')}
        aria-label={t('panels.searchTerminalOutput')}
        onChange={() => find('next')}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            find(e.shiftKey ? 'prev' : 'next');
          } else if (e.key === 'Escape') {
            e.preventDefault();
            onClose();
          }
        }}
      />
      <button
        className={toggleBtn(opts.caseSensitive)}
        title={t('panels.matchCase')}
        aria-pressed={opts.caseSensitive}
        onClick={() => {
          const next = { ...opts, caseSensitive: !opts.caseSensitive };
          setOpts(next);
          find('next', next);
        }}
      >
        Aa
      </button>
      <button
        className={toggleBtn(opts.wholeWord)}
        title={t('panels.wholeWord')}
        aria-pressed={opts.wholeWord}
        onClick={() => {
          const next = { ...opts, wholeWord: !opts.wholeWord };
          setOpts(next);
          find('next', next);
        }}
      >
        ⌗{t('panels.wholeWordLabel')}
      </button>
      <span
        className="min-w-10 text-center text-xs tabular-nums text-neutral-500"
        aria-live="polite"
        data-testid="search-count"
      >
        {results
          ? results.count === 0 || results.index < 0
            ? t('panels.noResults')
            : `${results.index + 1}/${results.count}`
          : ''}
      </span>
      <button
        className="px-1 text-xs text-neutral-400 hover:text-neutral-100"
        onClick={() => find('prev')}
        title={t('panels.previousMatch')}
        aria-label={t('panels.previous')}
      >
        ↑
      </button>
      <button
        className="px-1 text-xs text-neutral-400 hover:text-neutral-100"
        onClick={() => find('next')}
        title={t('panels.nextMatch')}
        aria-label={t('panels.next')}
      >
        ↓
      </button>
      <button
        className="px-1 text-xs text-neutral-400 hover:text-neutral-100"
        onClick={onClose}
        title={t('panels.closeSearchEsc')}
        aria-label={t('panels.closeSearch')}
      >
        ×
      </button>
    </div>
  );
}
