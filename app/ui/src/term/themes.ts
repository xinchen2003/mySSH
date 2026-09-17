//! 主题系统（M5）：内置 One Dark / Solarized / Nord 深浅色 + 自定义 JSON。
//! 主题 = xterm 调色板 + UI 明暗档 + chrome 派生色（deriveChromeVars 由调色板算出
//! 界面背景/文字/强调色刻度，经根节点 CSS 变量换肤；index.css 消费 --myssh-chrome-*）。
//! 跟随系统：settings theme.followSystem=true 时按 prefers-color-scheme 在深浅默认间切换。

import type { ITheme } from '@xterm/xterm';
import { tNow } from '../i18n';

export interface ThemeDef {
  id: string;
  label: string;
  /** UI 明暗档（chrome 换肤依据） */
  ui: 'dark' | 'light';
  xterm: ITheme;
}

export const BUILTIN_THEMES: ThemeDef[] = [
  {
    id: 'one-dark',
    label: 'One Dark',
    ui: 'dark',
    xterm: {
      background: '#282c34',
      foreground: '#abb2bf',
      cursor: '#528bff',
      selectionBackground: '#3e4451',
      black: '#3f4451',
      red: '#e05561',
      green: '#8cc265',
      yellow: '#d18f52',
      blue: '#4aa5f0',
      magenta: '#c162de',
      cyan: '#42b3c2',
      white: '#e6e6e6',
      brightBlack: '#4f5666',
      brightRed: '#ff616e',
      brightGreen: '#a5e075',
      brightYellow: '#f0a45d',
      brightBlue: '#6dc1ff',
      brightMagenta: '#de73ff',
      brightCyan: '#4cd7e8',
      brightWhite: '#ffffff',
    },
  },
  {
    id: 'solarized-dark',
    label: 'Solarized Dark',
    ui: 'dark',
    xterm: {
      background: '#002b36',
      foreground: '#839496',
      cursor: '#93a1a1',
      selectionBackground: '#073642',
      black: '#073642',
      red: '#dc322f',
      green: '#859900',
      yellow: '#b58900',
      blue: '#268bd2',
      magenta: '#d33682',
      cyan: '#2aa198',
      white: '#eee8d5',
      brightBlack: '#002b36',
      brightRed: '#cb4b16',
      brightGreen: '#586e75',
      brightYellow: '#657b83',
      brightBlue: '#839496',
      brightMagenta: '#6c71c4',
      brightCyan: '#93a1a1',
      brightWhite: '#fdf6e3',
    },
  },
  {
    id: 'solarized-light',
    label: 'Solarized Light',
    ui: 'light',
    xterm: {
      background: '#fdf6e3',
      foreground: '#586e75',
      cursor: '#657b83',
      selectionBackground: '#eee8d5',
      black: '#073642',
      red: '#dc322f',
      green: '#859900',
      yellow: '#b58900',
      blue: '#268bd2',
      magenta: '#d33682',
      cyan: '#2aa198',
      white: '#eee8d5',
      brightBlack: '#002b36',
      brightRed: '#cb4b16',
      brightGreen: '#586e75',
      brightYellow: '#657b83',
      brightBlue: '#839496',
      brightMagenta: '#6c71c4',
      brightCyan: '#93a1a1',
      brightWhite: '#fdf6e3',
    },
  },
  {
    id: 'nord',
    label: 'Nord',
    ui: 'dark',
    xterm: {
      background: '#2e3440',
      foreground: '#d8dee9',
      cursor: '#d8dee9',
      selectionBackground: '#434c5e',
      black: '#3b4252',
      red: '#bf616a',
      green: '#a3be8c',
      yellow: '#ebcb8b',
      blue: '#81a1c1',
      magenta: '#b48ead',
      cyan: '#88c0d0',
      white: '#e5e9f0',
      brightBlack: '#4c566a',
      brightRed: '#bf616a',
      brightGreen: '#a3be8c',
      brightYellow: '#ebcb8b',
      brightBlue: '#81a1c1',
      brightMagenta: '#b48ead',
      brightCyan: '#8fbcbb',
      brightWhite: '#eceff4',
    },
  },
  {
    id: 'midnight',
    label: '黑夜',
    ui: 'dark',
    xterm: {
      background: '#0d0d10',
      foreground: '#d6d6dd',
      cursor: '#e0e0e0',
      selectionBackground: '#2a2a33',
      black: '#1c1c1e',
      red: '#ff5f57',
      green: '#28c840',
      yellow: '#febc2e',
      blue: '#0a84ff',
      magenta: '#bf5af2',
      cyan: '#64d2ff',
      white: '#e5e5ea',
      brightBlack: '#3a3a3e',
      brightRed: '#ff7a72',
      brightGreen: '#4fe06a',
      brightYellow: '#ffd25e',
      brightBlue: '#409cff',
      brightMagenta: '#d07bff',
      brightCyan: '#8de0ff',
      brightWhite: '#ffffff',
    },
  },
  {
    id: 'github-light',
    label: 'GitHub',
    ui: 'light',
    xterm: {
      background: '#ffffff',
      foreground: '#1f2328',
      cursor: '#044289',
      selectionBackground: '#b6d0f5',
      black: '#6e7781',
      red: '#cf222e',
      green: '#116329',
      yellow: '#4d2d00',
      blue: '#0969da',
      magenta: '#8250df',
      cyan: '#1b7c83',
      white: '#57606a',
      brightBlack: '#57606a',
      brightRed: '#a40e26',
      brightGreen: '#1a7f37',
      brightYellow: '#633c01',
      brightBlue: '#218bff',
      brightMagenta: '#a475f9',
      brightCyan: '#3192aa',
      brightWhite: '#32383f',
    },
  },
  {
    id: 'eye-green',
    label: '护眼绿',
    ui: 'light',
    xterm: {
      background: '#c7edcc',
      foreground: '#1e3323',
      cursor: '#2f5233',
      selectionBackground: '#a3d8ab',
      black: '#33523a',
      red: '#8c3a2b',
      green: '#2d6a2e',
      yellow: '#6b5d1f',
      blue: '#2f5d8a',
      magenta: '#7a4a7d',
      cyan: '#2e7d6b',
      white: '#eaf5ea',
      brightBlack: '#4a6b52',
      brightRed: '#a34a38',
      brightGreen: '#3a7f3c',
      brightYellow: '#7d6e28',
      brightBlue: '#3a6d9e',
      brightMagenta: '#8d5a90',
      brightCyan: '#3a8f7c',
      brightWhite: '#ffffff',
    },
  },
  {
    id: 'warm',
    label: '暖阳',
    ui: 'light',
    xterm: {
      background: '#f4efe6',
      foreground: '#3a332a',
      cursor: '#8a6d3b',
      selectionBackground: '#e3d7bd',
      black: '#6b5f4e',
      red: '#b4432e',
      green: '#6f7d3f',
      yellow: '#9a6a1a',
      blue: '#4a6d8c',
      magenta: '#8c5450',
      cyan: '#5b7d72',
      white: '#f8f4ec',
      brightBlack: '#847660',
      brightRed: '#c45540',
      brightGreen: '#7f8f4c',
      brightYellow: '#b07c22',
      brightBlue: '#5a7fa0',
      brightMagenta: '#9e6560',
      brightCyan: '#6b8f83',
      brightWhite: '#fffdf8',
    },
  },
];

/** 系统明暗对应的默认主题 */
export const SYSTEM_DEFAULTS = { dark: 'one-dark', light: 'solarized-light' } as const;

export interface ThemeSettings {
  /** 'system' | 主题 id | 'custom' */
  theme: string;
  customJson?: string; // theme.custom：xterm ITheme JSON + ui 字段
}

/** 解析生效主题：system → 按媒体查询；custom → 解析 JSON（坏则回退 one-dark） */
export function resolveTheme(theme: string, customJson?: string): ThemeDef {
  let id = theme;
  if (id === 'system') {
    const dark = window.matchMedia('(prefers-color-scheme: dark)').matches;
    id = dark ? SYSTEM_DEFAULTS.dark : SYSTEM_DEFAULTS.light;
  }
  if (id === 'custom' && customJson) {
    try {
      const j = JSON.parse(customJson) as { ui?: 'dark' | 'light' } & ITheme;
      return {
        id: 'custom',
        label: tNow('state.themeCustom'),
        ui: j.ui === 'light' ? 'light' : 'dark',
        xterm: j,
      };
    } catch {
      // 坏 JSON 回退
    }
  }
  return BUILTIN_THEMES.find((t) => t.id === id) ?? BUILTIN_THEMES[0];
}

// ---------- chrome 派生（整体换肤） ----------
// 从 xterm 调色板推界面色：背景刻度由 background 向黑/白混合，文字刻度由
// foreground 生成，强调色刻度由 blue 生成。内置与自定义主题走同一逻辑。

/** 归一化为 #rrggbb；非法/缺省返回 null（自定义主题可能给 #rgb 或透明色） */
function normHex(c: string | undefined): string | null {
  if (!c) return null;
  const m = /^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.exec(c.trim());
  if (!m) return null;
  let h = m[1].toLowerCase();
  if (h.length === 3)
    h = h
      .split('')
      .map((x) => x + x)
      .join('');
  return `#${h}`;
}

/** a 向 b 混合 t（0-1，t=1 全为 b）；输入输出均为 #rrggbb */
function mixHex(a: string, b: string, t: number): string {
  const pa = parseInt(a.slice(1), 16);
  const pb = parseInt(b.slice(1), 16);
  const ch = (sa: number, sb: number): number => Math.round(sa + (sb - sa) * t);
  const r = ch((pa >> 16) & 0xff, (pb >> 16) & 0xff);
  const g = ch((pa >> 8) & 0xff, (pb >> 8) & 0xff);
  const bl = ch(pa & 0xff, pb & 0xff);
  return `#${((r << 16) | (g << 8) | bl).toString(16).padStart(6, '0')}`;
}

/** chrome CSS 变量名 → 色值。键与 index.css 的 --myssh-chrome-* 一一对应。 */
export function deriveChromeVars(def: ThemeDef): Record<string, string> {
  const bg = normHex(def.xterm.background) ?? (def.ui === 'light' ? '#ffffff' : '#000000');
  const fg = normHex(def.xterm.foreground) ?? (def.ui === 'light' ? '#000000' : '#ffffff');
  const accent = normHex(def.xterm.blue) ?? '#3b82f6';
  const dark = def.ui === 'dark';
  // 暗色：chrome 比终端更暗（终端是视觉焦点）；亮色：panel 比底色更亮（近白）。
  const toward = dark ? '#000000' : '#ffffff';
  const away = dark ? '#ffffff' : '#000000';
  return {
    '--myssh-chrome-body': mixHex(bg, toward, dark ? 0.55 : 0.05),
    '--myssh-chrome-panel': mixHex(bg, toward, dark ? 0.4 : 0.5),
    '--myssh-chrome-panel-2': mixHex(bg, toward, dark ? 0.2 : 0.08),
    '--myssh-chrome-deep': mixHex(bg, toward, dark ? 0.7 : 0.1),
    '--myssh-chrome-raised': mixHex(bg, away, dark ? 0.1 : 0.14),
    '--myssh-chrome-border': mixHex(bg, away, dark ? 0.14 : 0.14),
    '--myssh-chrome-hover': mixHex(bg, away, dark ? 0.06 : 0.06),
    '--myssh-chrome-text': mixHex(fg, away, dark ? 0.35 : 0.55),
    '--myssh-chrome-text-dim': dark ? fg : mixHex(fg, away, 0.2),
    '--myssh-chrome-text-faint': mixHex(fg, bg, dark ? 0.45 : 0.35),
    '--myssh-chrome-accent': accent,
    '--myssh-chrome-accent-hover': mixHex(accent, away, dark ? 0.12 : 0.08),
    '--myssh-chrome-accent-deep': mixHex(accent, dark ? toward : away, dark ? 0.3 : 0.15),
    '--myssh-chrome-accent-text': mixHex(accent, away, dark ? 0.35 : 0.25),
  };
}
