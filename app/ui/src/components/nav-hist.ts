/** 前进/后退历史栈（批次五 11.1）。
 *  纯函数便于单测；只有用户导航（进目录/上一级/路径栏跳转）才 push，
 *  刷新与 OSC 7 跟随不入栈。 */

export interface NavHist {
  back: string[];
  fwd: string[];
}

export const EMPTY_NAV_HIST: NavHist = { back: [], fwd: [] };

/** 用户导航到 next：当前路径入 back，前进栈清空 */
export function navPush(h: NavHist, current: string, next: string): NavHist {
  if (next === current) return h;
  return { back: [...h.back, current], fwd: [] };
}

/** 后退：取 back 栈顶；当前路径转存 fwd。栈空返回 null */
export function navBack(h: NavHist, current: string): { hist: NavHist; target: string } | null {
  const target = h.back[h.back.length - 1];
  if (target === undefined) return null;
  return { hist: { back: h.back.slice(0, -1), fwd: [...h.fwd, current] }, target };
}

/** 前进：取 fwd 栈顶；当前路径入 back。栈空返回 null */
export function navFwd(h: NavHist, current: string): { hist: NavHist; target: string } | null {
  const target = h.fwd[h.fwd.length - 1];
  if (target === undefined) return null;
  return { hist: { back: [...h.back, current], fwd: h.fwd.slice(0, -1) }, target };
}

/** 目标失效（目录被删）：仅出栈，路径不动 */
export function navDropBack(h: NavHist): NavHist {
  return { ...h, back: h.back.slice(0, -1) };
}

export function navDropFwd(h: NavHist): NavHist {
  return { ...h, fwd: h.fwd.slice(0, -1) };
}
/** 跟随终端目录的目标判定（批次六 10）：cwd 为空、与上次跟随一致（去重）、
 *  或与当前远程路径一致 → null（不导航）；否则返回 cwd。
 *  跟随导航属非用户导航，调用方不得入历史栈。 */
export function followTarget(
  lastFollowed: string | null,
  cwd: string | null,
  currentRemote: string,
): string | null {
  if (!cwd) return null;
  if (cwd === lastFollowed || cwd === currentRemote) return null;
  return cwd;
}
