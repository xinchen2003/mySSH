import { useRef } from 'react';
import type { LayoutNode } from '../term/layout';
import { useAppStore, type Tab } from '../state/app-store';
import { TerminalView } from '../terminal/TerminalView';

/**
 * 递归渲染分屏布局树。flexGrow= ratio/1-ratio 配合 flexBasis:0 实现占比。
 * 整树随 tab 显隐；pane 常驻挂载保活。
 */
export function SplitTree({
  tab,
  node,
  visible,
}: {
  tab: Tab;
  node: LayoutNode;
  visible: boolean;
}) {
  if (node.kind === 'leaf') {
    const pane = tab.panes[node.paneId];
    if (!pane) return null;
    return (
      // key=pane.id：分屏占比拖拽等同级重排时保持实例（leaf↔split 的跨层重挂由 TerminalView 保活池兜底）
      <PaneFrame key={node.paneId} tab={tab} paneId={node.paneId} visible={visible}>
        <TerminalView tab={tab} pane={pane} />
      </PaneFrame>
    );
  }
  return (
    <div
      className="flex h-full w-full"
      style={{ flexDirection: node.dir === 'row' ? 'row' : 'column' }}
    >
      <div
        className="min-h-0 min-w-0"
        style={{ flexGrow: node.ratio, flexBasis: 0, display: 'flex' }}
      >
        <SplitTree tab={tab} node={node.a} visible={visible} />
      </div>
      <Divider tabId={tab.id} node={node} />
      <div
        className="min-h-0 min-w-0"
        style={{ flexGrow: 1 - node.ratio, flexBasis: 0, display: 'flex' }}
      >
        <SplitTree tab={tab} node={node.b} visible={visible} />
      </div>
    </div>
  );
}

function PaneFrame({
  tab,
  paneId,
  visible,
  children,
}: {
  tab: Tab;
  paneId: string;
  visible: boolean;
  children: React.ReactNode;
}) {
  const setActivePane = useAppStore((s) => s.setActivePane);
  const closePane = useAppStore((s) => s.closePane);
  const active = tab.activePaneId === paneId;
  const multi = Object.keys(tab.panes).length > 1;
  return (
    <div
      className={`group relative h-full w-full ${multi ? (active ? 'ring-1 ring-blue-500/60' : 'ring-1 ring-neutral-800') : ''}`}
      style={{ display: visible ? undefined : 'none' }}
      onMouseDown={() => setActivePane(tab.id, paneId)}
    >
      {children}
      {/* 批次一 7.3：pane 关闭入口。absolute 悬浮不压缩终端空间；
          hover/focus-within 显示（键盘可达）；末叶走整标签关闭逻辑 */}
      <button
        className="absolute right-1 top-1 z-10 rounded bg-neutral-800/80 px-1.5 py-0.5 text-xs text-neutral-400 opacity-0 transition-opacity hover:bg-neutral-700 hover:text-neutral-100 focus-visible:opacity-100 group-focus-within:opacity-100 group-hover:opacity-100"
        title="关闭此窗格"
        aria-label="关闭此窗格"
        onClick={(e) => {
          e.stopPropagation();
          closePane(tab.id, paneId);
        }}
      >
        ×
      </button>
    </div>
  );
}

function Divider({ tabId, node }: { tabId: string; node: Extract<LayoutNode, { kind: 'split' }> }) {
  const setSplitRatio = useAppStore((s) => s.setSplitRatio);
  const ref = useRef<HTMLDivElement>(null);
  const horizontal = node.dir === 'row'; // 左右两栏 → 竖直分隔条

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.preventDefault();
    const bar = ref.current;
    const container = bar?.parentElement;
    if (!container) return;
    const rect = container.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      const ratio = horizontal
        ? (ev.clientX - rect.left) / rect.width
        : (ev.clientY - rect.top) / rect.height;
      setSplitRatio(tabId, node.id, ratio);
    };
    const up = () => {
      window.removeEventListener('pointermove', move);
      window.removeEventListener('pointerup', up);
    };
    window.addEventListener('pointermove', move);
    window.addEventListener('pointerup', up);
  };

  // 键盘可达：方向键以 0.05 步长调整占比（钳制复用 setSplitRatio 内的 setRatio）
  const onKeyDown = (e: React.KeyboardEvent<HTMLDivElement>) => {
    const dec = horizontal ? 'ArrowLeft' : 'ArrowUp';
    const inc = horizontal ? 'ArrowRight' : 'ArrowDown';
    if (e.key !== dec && e.key !== inc) return;
    e.preventDefault();
    setSplitRatio(tabId, node.id, node.ratio + (e.key === inc ? 0.05 : -0.05));
  };

  return (
    <div
      ref={ref}
      role="separator"
      tabIndex={0}
      aria-orientation={horizontal ? 'vertical' : 'horizontal'}
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
      className={`shrink-0 bg-neutral-700/60 transition-colors hover:bg-blue-500 focus-visible:ring-1 focus-visible:ring-neutral-500 ${
        horizontal ? 'w-1 cursor-col-resize' : 'h-1 cursor-row-resize'
      }`}
    />
  );
}
