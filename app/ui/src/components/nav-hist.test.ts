import { describe, expect, it } from 'vitest';
import {
  EMPTY_NAV_HIST,
  followTarget,
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
/** 批次六 10：跟随终端目录的目标判定（去重 / 非用户导航不入栈的前置过滤） */
describe('followTarget（终端 cwd 跟随）', () => {
  it('cwd 为空（shell 未开集成）不导航', () => {
    expect(followTarget(null, null, '/home/u')).toBeNull();
  });

  it('cwd 与当前远程路径一致不导航', () => {
    expect(followTarget(null, '/var', '/var')).toBeNull();
  });

  it('cwd 与上次跟随一致去重（1s 轮询不重复刷新）', () => {
    expect(followTarget('/var/log', '/var/log', '/etc')).toBeNull();
  });

  it('cwd 变化且不同当前路径 → 返回新目标', () => {
    expect(followTarget('/home/u', '/var/log', '/home/u')).toBe('/var/log');
  });

  it('用户手动离开后又回到 cwd：current 已一致，不重复导航', () => {
    // 场景：cwd=/a，用户导航到 /b（跟随把面板带回 /a），此时 current===cwd
    expect(followTarget('/b', '/a', '/a')).toBeNull();
  });
});
