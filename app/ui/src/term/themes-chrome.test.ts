//! deriveChromeVars 边界：非法/ shorthand 色值回退 + 深浅档派生方向。
import { describe, expect, it } from 'vitest';
import { BUILTIN_THEMES, deriveChromeVars, type ThemeDef } from './themes';

const def = (over: Partial<ThemeDef['xterm']>, ui: 'dark' | 'light'): ThemeDef => ({
  id: 't',
  label: 't',
  ui,
  xterm: over,
});

describe('deriveChromeVars', () => {
  it('暗色主题：chrome 背景比终端背景更暗', () => {
    const v = deriveChromeVars(def({ background: '#282c34' }, 'dark'));
    expect(parseInt(v['--myssh-chrome-body'].slice(1), 16)).toBeLessThan(0x282c34);
  });

  it('亮色主题：强调色刻度含主题 blue', () => {
    const v = deriveChromeVars(def({ blue: '#268bd2' }, 'light'));
    expect(v['--myssh-chrome-accent']).toBe('#268bd2');
  });

  it('#rgb shorthand 与非法色回退默认值，不产出 NaN', () => {
    const v = deriveChromeVars(
      def({ background: '#abc', foreground: 'transparent', blue: 'red' }, 'dark'),
    );
    for (const val of Object.values(v)) expect(val).toMatch(/^#[0-9a-f]{6}$/);
  });

  it('全部内置主题派生合法色值', () => {
    for (const t of BUILTIN_THEMES) {
      for (const val of Object.values(deriveChromeVars(t))) expect(val).toMatch(/^#[0-9a-f]{6}$/);
    }
  });
});
