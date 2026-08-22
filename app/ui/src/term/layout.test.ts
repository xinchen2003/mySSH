import { describe, expect, it } from 'vitest';
import { firstLeaf, leaf, paneIds, removeLeaf, setRatio, splitLeaf } from './layout';

describe('layout tree', () => {
  it('splitLeaf 把目标叶替换为 split(old,new) 且不动其它叶', () => {
    const t1 = leaf('p1');
    const t2 = splitLeaf(t1, 'p1', 'row', 'p2');
    expect(t2.kind).toBe('split');
    expect(paneIds(t2)).toEqual(['p1', 'p2']);

    const t3 = splitLeaf(t2, 'p2', 'col', 'p3');
    expect(paneIds(t3)).toEqual(['p1', 'p2', 'p3']);
    // 原树不可变
    expect(paneIds(t2)).toEqual(['p1', 'p2']);
    // 对不存在的叶操作 = 原树
    expect(splitLeaf(t3, 'nope', 'row', 'p4')).toBe(t3);
  });

  it('removeLeaf 折叠父 split；删空返回 null', () => {
    const t = splitLeaf(splitLeaf(leaf('p1'), 'p1', 'row', 'p2'), 'p2', 'col', 'p3');
    const r1 = removeLeaf(t, 'p3');
    expect(r1 && paneIds(r1)).toEqual(['p1', 'p2']);
    const r2 = r1 && removeLeaf(r1, 'p2');
    expect(r2 && paneIds(r2)).toEqual(['p1']);
    expect(r2 && removeLeaf(r2, 'p1')).toBeNull();
  });

  it('setRatio 钳制 0.1..0.9 且只改目标节点', () => {
    const t = splitLeaf(splitLeaf(leaf('p1'), 'p1', 'row', 'p2'), 'p2', 'col', 'p3');
    if (t.kind !== 'split') throw new Error('unreachable');
    const inner = t.b;
    if (inner.kind !== 'split') throw new Error('unreachable');
    const r = setRatio(t, inner.id, 1.5);
    if (r.kind !== 'split' || r.b.kind !== 'split') throw new Error('shape');
    expect(r.b.ratio).toBe(0.9);
    expect(r.ratio).toBe(0.5);
  });

  it('firstLeaf 取最左叶', () => {
    const t = splitLeaf(leaf('p1'), 'p1', 'row', 'p2');
    expect(firstLeaf(t)).toBe('p1');
  });
});
