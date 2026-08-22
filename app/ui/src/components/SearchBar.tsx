import { useEffect, useRef } from 'react';

/**
 * 终端搜索浮条（Ctrl+Shift+F 唤起）：输入即增量查找，
 * Enter 下一个 / Shift+Enter 上一个 / Esc 关闭。
 */
export function SearchBar({
  onFind,
  onClose,
}: {
  onFind: (query: string, dir: 'next' | 'prev') => void;
  onClose: () => void;
}) {
  const inputRef = useRef<HTMLInputElement>(null);
  useEffect(() => inputRef.current?.focus(), []);

  return (
    <div className="absolute top-1 right-3 z-10 flex items-center gap-1 rounded border border-neutral-700 bg-neutral-900/95 px-2 py-1 shadow-lg">
      <input
        ref={inputRef}
        className="w-48 rounded bg-neutral-800 px-2 py-0.5 text-xs text-neutral-200 outline-none"
        placeholder="搜索回滚…"
        onChange={(e) => onFind(e.target.value, 'next')}
        onKeyDown={(e) => {
          if (e.key === 'Enter') {
            e.preventDefault();
            onFind(inputRef.current?.value ?? '', e.shiftKey ? 'prev' : 'next');
          } else if (e.key === 'Escape') {
            e.preventDefault();
            onClose();
          }
        }}
      />
      <button
        className="px-1 text-xs text-neutral-400 hover:text-neutral-100"
        onClick={() => onFind(inputRef.current?.value ?? '', 'prev')}
        title="上一个 (Shift+Enter)"
      >
        ↑
      </button>
      <button
        className="px-1 text-xs text-neutral-400 hover:text-neutral-100"
        onClick={() => onFind(inputRef.current?.value ?? '', 'next')}
        title="下一个 (Enter)"
      >
        ↓
      </button>
      <button
        className="px-1 text-xs text-neutral-400 hover:text-neutral-100"
        onClick={onClose}
        title="关闭 (Esc)"
      >
        ×
      </button>
    </div>
  );
}
