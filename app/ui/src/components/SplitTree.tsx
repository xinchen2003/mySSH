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
      <PaneFrame tab={tab} paneId={node.paneId} visible={visible}>
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
  const active = tab.activePaneId === paneId;
  const multi = Object.keys(tab.panes).length > 1;
  return (
    <div
      className={`h-full w-full ${multi ? (active ? 'ring-1 ring-blue-500/60' : 'ring-1 ring-neutral-800') : ''}`}
      style={{ display: visible ? undefined : 'none' }}
      onMouseDown={() => setActivePane(tab.id, paneId)}
    >
      {children}
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

  return (
    <div
      ref={ref}
      onPointerDown={onPointerDown}
      className={`shrink-0 bg-neutral-700/60 transition-colors hover:bg-blue-500 ${
        horizontal ? 'w-1 cursor-col-resize' : 'h-1 cursor-row-resize'
      }`}
    />
  );
}
