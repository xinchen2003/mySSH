//! 服务器库分组逻辑（纯函数）：分组树构建、路径改写、可见序列拍平、虚拟分组过滤。
//! Sidebar 视图与 vitest 共用；不含任何 store/IPC 依赖。

import type { SessionRecord } from '../term/types';
/** settings KV 键（批次二服务器库） */
export const GROUP_KEYS = {
  clickToConnect: 'sidebar.clickToConnect',
  favorites: 'sessions.favorites',
  recent: 'sessions.recent',
  failed: 'sessions.failed',
  collapsed: 'groups.collapsed',
  extraGroups: 'groups.extra',
} as const;

export interface FailedEntry {
  id: string;
  message: string;
  ts: number;
}

/** KV 读 string[]（脏数据容忍：只收字符串） */
export function readStringList(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];
}

/** KV 读失败记录（脏数据容忍） */
export function readFailedList(v: unknown): FailedEntry[] {
  if (!Array.isArray(v)) return [];
  return v.filter(
    (x): x is FailedEntry =>
      !!x &&
      typeof x === 'object' &&
      typeof (x as FailedEntry).id === 'string' &&
      typeof (x as FailedEntry).message === 'string' &&
      typeof (x as FailedEntry).ts === 'number',
  );
}

export interface GroupTree {
  /** 子分组：名 → 子树 */
  groups: Map<string, GroupTree>;
  /** 本层直属会话 */
  items: SessionRecord[];
  /** 全路径（'' = 根） */
  path: string;
  /** 含子树的会话总数 */
  count: number;
}

/** 由会话 groupPath + 空分组集合构建树。相同路径去重；非法/空段忽略 */
export function buildGroupTree(sessions: SessionRecord[], extras: string[] = []): GroupTree {
  const root: GroupTree = { groups: new Map(), items: [], path: '', count: 0 };
  const ensure = (path: string): void => {
    let node = root;
    for (const seg of path.split('/')) {
      let child = node.groups.get(seg);
      if (!child) {
        child = {
          groups: new Map(),
          items: [],
          path: node.path ? `${node.path}/${seg}` : seg,
          count: 0,
        };
        node.groups.set(seg, child);
      }
      node = child;
    }
  };
  for (const p of extras) if (p && isValidGroupPath(p)) ensure(p);
  for (const s of sessions) {
    let node = root;
    if (s.groupPath) {
      ensure(s.groupPath);
      for (const seg of s.groupPath.split('/')) {
        const child = node.groups.get(seg);
        if (!child) break;
        node = child;
      }
    }
    node.items.push(s);
  }
  const fillCount = (node: GroupTree): number => {
    let n = node.items.length;
    for (const child of node.groups.values()) n += fillCount(child);
    node.count = n;
    return n;
  };
  fillCount(root);
  return root;
}

/** 与后端一致的分组路径校验（'' = 未分组根，合法） */
export function isValidGroupPath(path: string): boolean {
  if (path === '') return true;
  return path.split('/').every((seg) => seg !== '' && seg.trim() === seg);
}

/** 循环防护（拖拽/重命名共用）：目标不得是源本身或落在源子树内 */
export function canMoveGroup(source: string, target: string): boolean {
  if (!source) return false;
  if (target === source) return false;
  return !target.startsWith(source + '/');
}

/** 路径前缀改写：rename 时同步 extras/collapsed 等路径集合。去重保序 */
export function rewritePathOnRename(paths: string[], old: string, newPrefix: string): string[] {
  const out = paths.map((p) => {
    if (p === old) return newPrefix;
    if (p.startsWith(old + '/'))
      return newPrefix ? newPrefix + p.slice(old.length) : p.slice(old.length + 1);
    return p;
  });
  // ''（未分组根）不是分组条目：改名到根即从集合移除
  return [...new Set(out)].filter((p) => p !== '' && isValidGroupPath(p));
}

/** 删除分组（保留会话）时的路径集合改写：该路径移除；子路径上移父级 */
export function rewritePathOnDeleteKeep(paths: string[], path: string): string[] {
  const parent = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
  const out: string[] = [];
  for (const p of paths) {
    if (p === path) continue; // 被删分组本身移除
    if (p.startsWith(path + '/')) {
      const rel = p.slice(path.length + 1);
      out.push(parent ? `${parent}/${rel}` : rel);
    } else {
      out.push(p);
    }
  }
  return [...new Set(out)];
}

/** 可见序列拍平（折叠的分组不展开其内容）：Shift 连选与键盘导航基于此顺序 */
export interface VisibleRow {
  kind: 'group' | 'session';
  /** group → 分组全路径；session → 会话 id */
  key: string;
  depth: number;
  session?: SessionRecord;
  group?: GroupTree;
}

export function flattenVisible(root: GroupTree, collapsed: ReadonlySet<string>): VisibleRow[] {
  const out: VisibleRow[] = [];
  const walk = (node: GroupTree, depth: number) => {
    for (const s of node.items) out.push({ kind: 'session', key: s.id, depth, session: s });
    for (const [, child] of [...node.groups.entries()].sort((a, b) =>
      a[0].localeCompare(b[0], 'zh-CN'),
    )) {
      out.push({ kind: 'group', key: child.path, depth, group: child });
      if (!collapsed.has(child.path)) walk(child, depth + 1);
    }
  };
  walk(root, 0);
  return out;
}

/** Shift 连选：可见会话 id 序列中 anchor→target 的闭区间（顺序无关） */
export function rangeSessionIds(visibleIds: string[], anchor: string, target: string): string[] {
  const a = visibleIds.indexOf(anchor);
  const b = visibleIds.indexOf(target);
  if (a < 0 || b < 0) return [target];
  const [lo, hi] = a <= b ? [a, b] : [b, a];
  return visibleIds.slice(lo, hi + 1);
}

/** 全部可选分组路径（移动到分组对话框 datalist 用），按字典序 */
export function collectGroupPaths(root: GroupTree): string[] {
  const out: string[] = [];
  const walk = (node: GroupTree) => {
    for (const child of node.groups.values()) {
      out.push(child.path);
      walk(child);
    }
  };
  walk(root);
  return out.sort((a, b) => a.localeCompare(b, 'zh-CN'));
}

// ---------- 虚拟分组（8.5）：纯过滤，不改真实归属 ----------

/** 虚拟过滤视图：目前仅「收藏」；退出过滤（null）即回到全量分组树 */

export interface VirtualCtx {
  /** 收藏会话 id */
  favorites: ReadonlySet<string>;
}

export function virtualCount(sessions: SessionRecord[], ctx: VirtualCtx): number {
  return filterVirtual(sessions, ctx).length;
}
/** 生成 ssh 命令行（含 -J 跳板链与 -i 私钥）；跳板档案缺失的跳跳过 */
export function sshCommand(rec: SessionRecord, all: SessionRecord[]): string {
  const parts = ['ssh'];
  if (rec.jumpChain.length > 0) {
    const hops = rec.jumpChain
      .map((id) => all.find((s) => s.id === id))
      .filter((s): s is SessionRecord => !!s)
      .map((s) => `${s.username}@${s.host}:${s.port}`);
    if (hops.length > 0) parts.push('-J', hops.join(','));
  }
  if (rec.keyPath) parts.push('-i', rec.keyPath);
  parts.push('-p', String(rec.port), `${rec.username}@${rec.host}`);
  return parts.join(' ');
}

export function filterVirtual(sessions: SessionRecord[], ctx: VirtualCtx): SessionRecord[] {
  return sessions.filter((s) => ctx.favorites.has(s.id));
}
