import { describe, expect, it } from 'vitest';
import {
  buildGroupTree,
  canMoveGroup,
  collectGroupPaths,
  filterVirtual,
  flattenVisible,
  isValidGroupPath,
  rangeSessionIds,
  readFailedList,
  readStringList,
  rewritePathOnDeleteKeep,
  rewritePathOnRename,
  sshCommand,
  virtualCount,
} from './groups';
import type { SessionRecord } from '../term/types';

function rec(id: string, groupPath: string, over: Partial<SessionRecord> = {}): SessionRecord {
  return {
    id,
    name: `srv-${id}`,
    host: `h${id}.example.com`,
    port: 22,
    username: 'ops',
    authType: 'password',
    jumpChain: [],
    groupPath,
    tags: [],
    createdAt: '',
    updatedAt: '',
    ...over,
  };
}

describe('分组树构建（含空分组与计数）', () => {
  it('按 groupPath 嵌套；空分组（extras）也出现在树中；count 含子树', () => {
    const tree = buildGroupTree(
      [rec('1', '生产/华东'), rec('2', '生产/华南'), rec('3', ''), rec('4', '生产/华东/核心')],
      ['空分组/待用', '生产'], // '生产' 与既有路径去重
    );
    expect(tree.count).toBe(4);
    expect(tree.items.map((s) => s.id)).toEqual(['3']); // 未分组在根
    const prod = tree.groups.get('生产');
    expect(prod?.count).toBe(3);
    expect(prod?.groups.get('华东')?.count).toBe(2);
    expect(prod?.groups.get('华东')?.groups.get('核心')?.items[0]?.id).toBe('4');
    // 空分组占位
    expect(tree.groups.get('空分组')?.groups.get('待用')?.count).toBe(0);
  });

  it('非法 extras 路径被忽略', () => {
    const tree = buildGroupTree([], ['a//b', ' ok ']);
    expect(collectGroupPaths(tree)).toEqual([]);
  });
});

describe('路径校验与循环防护', () => {
  it('isValidGroupPath', () => {
    expect(isValidGroupPath('')).toBe(true);
    expect(isValidGroupPath('a/b')).toBe(true);
    expect(isValidGroupPath('a//b')).toBe(false);
    expect(isValidGroupPath('a /b')).toBe(false);
  });

  it('canMoveGroup：自身与子树拒绝', () => {
    expect(canMoveGroup('a/b', 'c')).toBe(true);
    expect(canMoveGroup('a/b', '')).toBe(true);
    expect(canMoveGroup('a/b', 'a/b')).toBe(false);
    expect(canMoveGroup('a/b', 'a/b/c')).toBe(false);
    expect(canMoveGroup('', 'a')).toBe(false); // 根不可移动
  });
});

describe('路径集合改写（extras/collapsed 同步）', () => {
  it('rename 改写前缀并去重', () => {
    expect(rewritePathOnRename(['a/b', 'a/b/c', 'x'], 'a/b', 'd')).toEqual(['d', 'd/c', 'x']);
    // 改为根：子路径提升为顶级
    expect(rewritePathOnRename(['a/b', 'a/b/c'], 'a/b', '')).toEqual(['c']);
    // 与现有路径撞名 → 去重合并
    expect(rewritePathOnRename(['a', 'd/c'], 'a', 'd')).toEqual(['d', 'd/c']);
  });

  it('删除保留会话：该路径移除、子路径上移父级', () => {
    expect(rewritePathOnDeleteKeep(['a/b', 'a/b/c', 'a/x'], 'a/b')).toEqual(['a/c', 'a/x']);
    // 顶级分组的子组提升为根级
    expect(rewritePathOnDeleteKeep(['a/b'], 'a')).toEqual(['b']);
  });
});

describe('可见序列与 Shift 连选', () => {
  it('折叠的分组不展开内容；连选取可见闭区间', () => {
    const tree = buildGroupTree([rec('1', 'g/a'), rec('2', 'g/b'), rec('3', 'h'), rec('4', '')]);
    const open = flattenVisible(tree, new Set());
    const openIds = open.filter((r) => r.kind === 'session').map((r) => r.key);
    expect(openIds).toEqual(['4', '1', '2', '3']); // 根直属在前，分组按字典序

    // 折叠 g：其成员不可见
    const shut = flattenVisible(tree, new Set(['g']));
    const shutIds = shut.filter((r) => r.kind === 'session').map((r) => r.key);
    expect(shutIds).toEqual(['4', '3']);

    // Shift 连选（可见序 4,1,2,3 中 1→3 = [1,2,3]；反向同区间）
    expect(rangeSessionIds(openIds, '1', '3')).toEqual(['1', '2', '3']);
    expect(rangeSessionIds(openIds, '3', '1')).toEqual(['1', '2', '3']);
    expect(rangeSessionIds(openIds, '不存在', '2')).toEqual(['2']);
  });
});

describe('虚拟分组过滤', () => {
  const sessions = [rec('1', 'g'), rec('2', ''), rec('3', 'g')];
  const ctx = {
    favorites: new Set(['1']),
    recent: ['3', '1', '已被删的'],
    online: new Set(['2']),
    failed: new Map([['3', '超时']]),
  };

  it('收藏/最近/在线/未分组/失败', () => {
    expect(filterVirtual('favorites', sessions, ctx).map((s) => s.id)).toEqual(['1']);
    // 最近连接：按记录顺序，已删档案跳过
    expect(filterVirtual('recent', sessions, ctx).map((s) => s.id)).toEqual(['3', '1']);
    expect(filterVirtual('online', sessions, ctx).map((s) => s.id)).toEqual(['2']);
    expect(filterVirtual('ungrouped', sessions, ctx).map((s) => s.id)).toEqual(['2']);
    expect(filterVirtual('failed', sessions, ctx).map((s) => s.id)).toEqual(['3']);
  });
  it('「默认」过滤：仅 groupPath 为空串的会话；嵌套分组不混入', () => {
    const list = [rec('a', '生产'), rec('b', ''), rec('c', '生产/华东'), rec('d', '')];
    expect(filterVirtual('ungrouped', list, ctx).map((s) => s.id)).toEqual(['b', 'd']);
    expect(virtualCount('ungrouped', list, ctx)).toBe(2);
    // 空集合/全已分组时为 0，不抛错
    expect(virtualCount('ungrouped', [rec('x', 'g')], ctx)).toBe(0);
  });
});

describe('KV 读取容忍脏数据', () => {
  it('readStringList / readFailedList 过滤非法项', () => {
    expect(readStringList(['a', 1, 'b', null])).toEqual(['a', 'b']);
    expect(readStringList('nope')).toEqual([]);
    expect(
      readFailedList([{ id: 'a', message: 'm', ts: 1 }, { id: 2 }, 'x', null]).map((f) => f.id),
    ).toEqual(['a']);
    expect(readFailedList(undefined)).toEqual([]);
  });
});

describe('sshCommand', () => {
  it('含跳板与私钥', () => {
    const jump = rec('j1', '', { name: 'bastion', host: '10.0.0.1', username: 'root', port: 2222 });
    const target = rec('t', '', {
      host: 'web.internal',
      username: 'ops',
      port: 22,
      jumpChain: ['j1', 'missing'],
      keyPath: 'C:/keys/id.pem',
    });
    expect(sshCommand(target, [jump, target])).toBe(
      'ssh -J root@10.0.0.1:2222 -i C:/keys/id.pem -p 22 ops@web.internal',
    );
  });

  it('直连无额外参数', () => {
    expect(sshCommand(rec('a', ''), [])).toBe('ssh -p 22 ops@ha.example.com');
  });
});
