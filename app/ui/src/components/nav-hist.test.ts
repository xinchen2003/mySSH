import { describe, expect, it } from 'vitest';
import {
  EMPTY_NAV_HIST,
  navBack,
  navDropBack,
  navFwd,
  navPush,
  type NavHist,
} from './nav-hist';

/** 批次五 11.1：路径栏前进/后退历史语义 */

describe('navPush（用户导航）', () => {
  it('当前路径入 back，前进栈清空', () => {
    const h: NavHist = { back: ['/a'], fwd: ['/z'] };
    const next = navPush(h, '/b', '/c');
    expect(next).toEqual({ back: ['/a', '/b'], fwd: [] });
  });

  it('原地导航（同路径）不入栈', () => {
    const h: NavHist = { back: ['/a'], fwd: [] };
    expect(navPush(h, '/b', '/b')).toBe(h);
  });
});

describe('navBack / navFwd', () => {
  it('后退：back 出栈，当前路径转存 fwd', () => {
    const h: NavHist = { back: ['/a', '/b'], fwd: [] };
    const r = navBack(h, '/c');
    expect(r?.target).toBe('/b');
    expect(r?.hist).toEqual({ back: ['/a'], fwd: ['/c'] });
  });

  it('前进：fwd 出栈，当前路径入 back', () => {
    const h: NavHist = { back: ['/a'], fwd: ['/c'] };
    const r = navFwd(h, '/b');
    expect(r?.target).toBe('/c');
    expect(r?.hist).toEqual({ back: ['/a', '/b'], fwd: [] });
  });

  it('空栈返回 null', () => {
    expect(navBack(EMPTY_NAV_HIST, '/')).toBeNull();
    expect(navFwd(EMPTY_NAV_HIST, '/')).toBeNull();
  });

  it('后退再前进可往返', () => {
    let h = navPush(EMPTY_NAV_HIST, '/', '/var');
    h = navPush(h, '/var', '/var/log');
    const b = navBack(h, '/var/log');
    expect(b?.target).toBe('/var');
    const f = navFwd(b?.hist ?? EMPTY_NAV_HIST, '/var');
    expect(f?.target).toBe('/var/log');
  });

  it('目标失效仅出栈', () => {
    const h: NavHist = { back: ['/a', '/gone'], fwd: [] };
    expect(navDropBack(h)).toEqual({ back: ['/a'], fwd: [] });
  });
});
