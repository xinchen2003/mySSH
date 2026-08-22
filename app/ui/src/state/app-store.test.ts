import { describe, expect, it } from 'vitest';
import { useAppStore } from './app-store';
import type { TermOpenSpec } from '../term/types';

const spec: TermOpenSpec = {
  host: 'h',
  port: 22,
  user: 'u',
  auth: { type: 'agent' },
};

describe('app-store tabs', () => {
  it('moveTab 把拖拽标签移到目标之前，顺序其余不变', () => {
    const s = useAppStore.getState();
    s.connect(spec);
    s.connect(spec);
    s.connect(spec);
    const ids = useAppStore.getState().tabs.map((t) => t.id);
    expect(ids).toHaveLength(3);

    // 第三个移到第一个之前
    s.moveTab(ids[2], ids[0]);
    const after = useAppStore.getState().tabs.map((t) => t.id);
    expect(after).toEqual([ids[2], ids[0], ids[1]]);

    // 自移/未知 id 无副作用
    s.moveTab(ids[2], ids[2]);
    expect(useAppStore.getState().tabs.map((t) => t.id)).toEqual(after);
    s.moveTab('nope', ids[0]);
    expect(useAppStore.getState().tabs.map((t) => t.id)).toEqual(after);
  });
});
