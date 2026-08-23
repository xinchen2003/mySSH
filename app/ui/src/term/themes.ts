//! 主题系统（M5）：内置 One Dark / Solarized / Nord 深浅色 + 自定义 JSON。
//! 主题 = xterm 调色板 + UI 明暗档（chrome 经 index.css 的 [data-ui] 覆盖层换肤）。
//! 跟随系统：settings theme.followSystem=true 时按 prefers-color-scheme 在深浅默认间切换。

import type { ITheme } from '@xterm/xterm';

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
      return { id: 'custom', label: '自定义', ui: j.ui === 'light' ? 'light' : 'dark', xterm: j };
    } catch {
      // 坏 JSON 回退
    }
  }
  return BUILTIN_THEMES.find((t) => t.id === id) ?? BUILTIN_THEMES[0];
}
