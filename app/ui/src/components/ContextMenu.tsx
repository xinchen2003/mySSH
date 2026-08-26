import { useCallback, useEffect, useRef, useState } from 'react';

export interface MenuEntry {
  label: string;
  /** 可选图标槽（emoji/文本图标），尺寸跟随 --myssh-menu-icon */
  icon?: string;
  danger?: boolean;
  disabled?: boolean;
  onSelect: () => void;
}

export type MenuItem = MenuEntry | 'separator';

/**
 * 通用右键菜单（侧栏服务器/分组先用；批次四复用到标签与终端）。
 * 语义：Esc 关闭；↑↓ 循环移动（跳过分隔线与禁用项）；Enter 执行；
 * 点击外部关闭；渲染后测量并收拢进窗口边界；卸载时清理全部监听器。
 */
export function ContextMenu({
  x,
  y,
  items,
  onClose,
}: {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });
  const actionable = items
    .map((it, i) => ({ it, i }))
    .filter((x): x is { it: MenuEntry; i: number } => x.it !== 'separator' && !x.it.disabled);
  const [active, setActive] = useState<number>(() => actionable[0]?.i ?? -1);

  // 边界收拢：渲染后测量，超出窗口则左/上移
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const nx = Math.min(x, window.innerWidth - r.width - 4);
    const ny = Math.min(y, window.innerHeight - r.height - 4);
    setPos({ x: Math.max(4, nx), y: Math.max(4, ny) });
  }, [x, y]);

  const run = useCallback(
    (idx: number) => {
      const it = items[idx];
      if (!it || it === 'separator' || it.disabled) return;
      onClose();
      it.onSelect();
    },
    [items, onClose],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
        return;
      }
      if (actionable.length === 0) return;
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
        e.preventDefault();
        e.stopPropagation();
        const cur = actionable.findIndex((a) => a.i === active);
        const step = e.key === 'ArrowDown' ? 1 : -1;
        const next = actionable[(cur + step + actionable.length) % actionable.length];
        setActive(next.i);
      } else if (e.key === 'Enter' && active >= 0) {
        e.preventDefault();
        e.stopPropagation();
        run(active);
      }
    };
    const onPointerDown = (e: PointerEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    };
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('pointerdown', onPointerDown, true);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('pointerdown', onPointerDown, true);
    };
  }, [actionable, active, run, onClose]);

  return (
    <div
      ref={ref}
      role="menu"
      className="fixed z-50 min-w-40 rounded border border-neutral-700 bg-neutral-900 py-1 shadow-xl"
      style={{ left: pos.x, top: pos.y }}
    >
      {items.map((it, i) =>
        it === 'separator' ? (
          <div key={i} className="my-1 border-t border-neutral-800" />
        ) : (
          <button
            key={i}
            role="menuitem"
            aria-disabled={it.disabled || undefined}
            disabled={it.disabled}
            className={`flex w-full items-center gap-2 px-3 py-1.5 text-left ${
              it.danger
                ? 'text-red-400 hover:bg-neutral-800'
                : 'text-neutral-200 hover:bg-neutral-800'
            } ${i === active ? 'bg-neutral-800' : ''} ${it.disabled ? 'opacity-40' : ''}`}
            style={{ fontSize: 'var(--myssh-menu-font, 12px)' }}
            onMouseEnter={() => !it.disabled && setActive(i)}
            onClick={() => run(i)}
          >
            {it.icon !== undefined && (
              <span
                aria-hidden
                className="shrink-0 text-center text-neutral-400"
                style={{
                  width: 'var(--myssh-menu-icon, 14px)',
                  height: 'var(--myssh-menu-icon, 14px)',
                  fontSize: 'var(--myssh-menu-icon, 14px)',
                  lineHeight: 'var(--myssh-menu-icon, 14px)',
                }}
              >
                {it.icon}
              </span>
            )}
            <span className="min-w-0 flex-1 truncate">{it.label}</span>
          </button>
        ),
      )}
    </div>
  );
}
