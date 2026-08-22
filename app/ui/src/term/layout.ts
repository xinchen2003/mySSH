/**
 * 分屏布局树：不可变更新。叶 = 一个终端 pane；split = 行/列二分 + 占比。
 * 每个 split 节点带稳定 id（divider 拖拽按 id 寻址，避免路径编码）。
 */
export type LayoutNode =
  | { kind: 'leaf'; paneId: string }
  | {
      kind: 'split';
      id: string;
      dir: 'row' | 'col';
      a: LayoutNode;
      b: LayoutNode;
      /** a 的占比 0.1..0.9 */
      ratio: number;
    };

let splitSeq = 1;

export function leaf(paneId: string): LayoutNode {
  return { kind: 'leaf', paneId };
}

/** 把某叶替换为 split(old, new)，返回新树 */
export function splitLeaf(
  root: LayoutNode,
  paneId: string,
  dir: 'row' | 'col',
  newPaneId: string,
): LayoutNode {
  if (root.kind === 'leaf') {
    if (root.paneId !== paneId) return root;
    return {
      kind: 'split',
      id: `sp${splitSeq++}`,
      dir,
      a: root,
      b: leaf(newPaneId),
      ratio: 0.5,
    };
  }
  const a = splitLeaf(root.a, paneId, dir, newPaneId);
  if (a !== root.a) return { ...root, a };
  const b = splitLeaf(root.b, paneId, dir, newPaneId);
  if (b !== root.b) return { ...root, b };
  return root;
}

/** 摘除某叶并把父 split 折叠为兄弟子树；树空返回 null */
export function removeLeaf(root: LayoutNode, paneId: string): LayoutNode | null {
  if (root.kind === 'leaf') return root.paneId === paneId ? null : root;
  if (root.a.kind === 'leaf' && root.a.paneId === paneId) return root.b;
  if (root.b.kind === 'leaf' && root.b.paneId === paneId) return root.a;
  const a = removeLeaf(root.a, paneId);
  if (a !== root.a) return a === null ? root.b : { ...root, a };
  const b = removeLeaf(root.b, paneId);
  if (b !== root.b) return b === null ? root.a : { ...root, b };
  return root;
}

/** 调整某 split 节点占比（钳制 0.1..0.9） */
export function setRatio(root: LayoutNode, splitId: string, ratio: number): LayoutNode {
  const clamped = Math.min(0.9, Math.max(0.1, ratio));
  if (root.kind === 'leaf') return root;
  if (root.id === splitId) return { ...root, ratio: clamped };
  return { ...root, a: setRatio(root.a, splitId, ratio), b: setRatio(root.b, splitId, ratio) };
}

/** 遍历所有 paneId（渲染与计数用） */
export function paneIds(root: LayoutNode): string[] {
  return root.kind === 'leaf' ? [root.paneId] : [...paneIds(root.a), ...paneIds(root.b)];
}

/** 找第一个叶（关闭后聚焦兜底） */
export function firstLeaf(root: LayoutNode): string {
  return root.kind === 'leaf' ? root.paneId : firstLeaf(root.a);
}
