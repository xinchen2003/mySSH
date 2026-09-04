import { useCallback, useEffect, useRef, useState } from 'react';

export interface MenuEntry {
  label: string;
  /** 可选图标槽（emoji/文本图标），尺寸跟随 --myssh-menu-icon */
  icon?: string;
  danger?: boolean;
  disabled?: boolean;
  /** 子菜单项；设置后 onSelect 不触发，Enter/→/悬停展开子菜单 */
  children?: MenuItem[];
  onSelect?: () => void;
}

export type MenuItem = MenuEntry | 'separator';

/** 过滤出可行动项（非分隔线、未禁用） */
function actionableOf(list: MenuItem[]): { it: MenuEntry; i: number }[] {
  const out: { it: MenuEntry; i: number }[] = [];
  list.forEach((it, i) => {
    if (it !== 'separator' && !it.disabled) out.push({ it, i });
  });
  return out;
}

/** 单个菜单项（主菜单与子菜单共用） */
function MenuButton({
  it,
  itemId,
  active,
  onHover,
  onClick,
}: {
  it: MenuEntry;
  itemId: string;
  active: boolean;
  onHover: () => void;
  onClick: () => void;
}) {
  return (
    <button
      role="menuitem"
      id={itemId}
      aria-disabled={it.disabled || undefined}
      aria-haspopup={it.children ? 'menu' : undefined}
      disabled={it.disabled}
      className={`flex w-full items-center gap-2 px-3 py-1.5 text-left ${
        it.danger ? 'text-red-400 hover:bg-neutral-800' : 'text-neutral-200 hover:bg-neutral-800'
      } ${active ? 'bg-neutral-800' : ''} ${it.disabled ? 'opacity-40' : ''}`}
      style={{ fontSize: 'var(--myssh-menu-font, 12px)' }}
      onMouseEnter={onHover}
      onClick={onClick}
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
      {it.children && (
        <span aria-hidden className="shrink-0 text-neutral-500">
          ▸
        </span>
      )}
    </button>
  );
}

/**
 * 通用右键菜单（侧栏服务器/分组先用；批次四复用到标签与终端）。
 * 语义：Esc 关闭；↑↓ 循环移动（跳过分隔线与禁用项）；Enter 执行；
 * 带子菜单的项 Enter/→/悬停展开子菜单，← 收回；子菜单内 ↑↓/Enter 同级语义；
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
  const subRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ x, y });
  const actionable = actionableOf(items);
  const [active, setActive] = useState<number>(() => actionable[0]?.i ?? -1);
  /** 已展开子菜单的父项下标 */
  const [subOpen, setSubOpen] = useState<number | null>(null);
  /** 子菜单内当前项（子项数组下标） */
  const [subActive, setSubActive] = useState(-1);
  /** 子菜单弹出位置（测量后写入；forIdx 匹配当前展开项才生效，否则隐藏避免闪跳） */
  const [subPos, setSubPos] = useState<{ forIdx: number; sx: number; sy: number } | null>(null);
  const subItems = subOpen !== null ? (items[subOpen] as MenuEntry).children : undefined;
  const subActionable = subItems ? actionableOf(subItems) : [];

  // 边界收拢：渲染后测量，超出窗口则左/上移
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    const nx = Math.min(x, window.innerWidth - r.width - 4);
    const ny = Math.min(y, window.innerHeight - r.height - 4);
    setPos({ x: Math.max(4, nx), y: Math.max(4, ny) });
  }, [x, y]);
  // 打开即聚焦容器（tabIndex=-1），配合 aria-activedescendant 暴露当前项
  useEffect(() => {
    ref.current?.focus();
  }, []);

  const openSub = useCallback(
    (idx: number) => {
      const it = items[idx];
      if (!it || it === 'separator' || !it.children) return;
      setSubOpen(idx);
      setSubActive(actionableOf(it.children)[0]?.i ?? -1);
    },
    [items],
  );

  // 子菜单定位：展开后测量，贴父项右缘；水平越界翻到左侧，垂直越界上移收拢
  useEffect(() => {
    if (subOpen === null) return;
    const parentBtn = document.getElementById(`myssh-menu-item-${subOpen}`);
    const menu = ref.current;
    const sub = subRef.current;
    if (!parentBtn || !menu || !sub) return;
    const pr = parentBtn.getBoundingClientRect();
    const mr = menu.getBoundingClientRect();
    const sr = sub.getBoundingClientRect();
    let sx = mr.right - 2;
    if (sx + sr.width > window.innerWidth - 4) sx = mr.left - sr.width + 2;
    const sy = Math.min(pr.top - 5, window.innerHeight - sr.height - 4);
    setSubPos({ forIdx: subOpen, sx: Math.max(4, sx), sy: Math.max(4, sy) });
  }, [subOpen]);

  const run = useCallback(
    (idx: number) => {
      const it = items[idx];
      if (!it || it === 'separator' || it.disabled) return;
      if (it.children) {
        openSub(idx);
        return;
      }
      onClose();
      it.onSelect?.();
    },
    [items, onClose, openSub],
  );

  const runSub = useCallback(
    (idx: number) => {
      const it = subItems?.[idx];
      if (!it || it === 'separator' || it.disabled) return;
      onClose();
      it.onSelect?.();
    },
    [subItems, onClose],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        onClose();
        return;
      }
      // 子菜单打开时：↑↓/Enter 作用于子菜单，← 收回
      if (subOpen !== null) {
        if (e.key === 'ArrowLeft') {
          e.preventDefault();
          e.stopPropagation();
          setSubOpen(null);
          return;
        }
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
          if (subActionable.length === 0) return;
          e.preventDefault();
          e.stopPropagation();
          const cur = subActionable.findIndex((a) => a.i === subActive);
          const step = e.key === 'ArrowDown' ? 1 : -1;
          const next = subActionable[(cur + step + subActionable.length) % subActionable.length];
          setSubActive(next.i);
        } else if (e.key === 'Enter' && subActive >= 0) {
          e.preventDefault();
          e.stopPropagation();
          runSub(subActive);
        }
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
      } else if (e.key === 'ArrowRight' && active >= 0) {
        const it = items[active];
        if (it && it !== 'separator' && it.children) {
          e.preventDefault();
          e.stopPropagation();
          openSub(active);
        }
      } else if (e.key === 'Enter' && active >= 0) {
        e.preventDefault();
        e.stopPropagation();
        run(active);
      }
    };
    const onPointerDown = (e: PointerEvent) => {
      const t = e.target as Node;
      const inMain = ref.current?.contains(t) ?? false;
      const inSub = subRef.current?.contains(t) ?? false;
      if (!inMain && !inSub) onClose();
    };
    window.addEventListener('keydown', onKey, true);
    window.addEventListener('pointerdown', onPointerDown, true);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      window.removeEventListener('pointerdown', onPointerDown, true);
    };
  }, [actionable, active, run, onClose, subOpen, subActive, subActionable, items, openSub, runSub]);

  return (
    <div
      ref={ref}
      role="menu"
      tabIndex={-1}
      aria-activedescendant={active >= 0 ? `myssh-menu-item-${active}` : undefined}
      className="fixed z-50 min-w-40 rounded outline-none border border-neutral-700 bg-neutral-900 py-1 shadow-xl"
      style={{ left: pos.x, top: pos.y }}
    >
      {items.map((it, i) =>
        it === 'separator' ? (
          <div key={i} role="separator" className="my-1 border-t border-neutral-800" />
        ) : (
          <div key={i} className="relative">
            <MenuButton
              it={it}
              itemId={`myssh-menu-item-${i}`}
              active={i === active}
              onHover={() => {
                if (it.disabled) return;
                setActive(i);
                if (it.children) {
                  if (subOpen !== i) {
                    setSubOpen(i);
                    setSubActive(actionableOf(it.children)[0]?.i ?? -1);
                  }
                } else if (subOpen !== null) {
                  setSubOpen(null);
                }
              }}
              onClick={() => run(i)}
            />
            {it.children && subOpen === i && (
              <div
                ref={subRef}
                role="menu"
                className="z-50 min-w-36 rounded border border-neutral-700 bg-neutral-900 py-1 shadow-xl"
                style={
                  subPos && subPos.forIdx === i
                    ? { position: 'fixed', left: subPos.sx, top: subPos.sy }
                    : { position: 'fixed', left: 0, top: 0, visibility: 'hidden' }
                }
              >
                {it.children.map((sub, j) =>
                  sub === 'separator' ? (
                    <div key={j} role="separator" className="my-1 border-t border-neutral-800" />
                  ) : (
                    <MenuButton
                      key={j}
                      it={sub}
                      itemId={`myssh-subitem-${j}`}
                      active={j === subActive}
                      onHover={() => !sub.disabled && setSubActive(j)}
                      onClick={() => runSub(j)}
                    />
                  ),
                )}
              </div>
            )}
          </div>
        ),
      )}
    </div>
  );
}
