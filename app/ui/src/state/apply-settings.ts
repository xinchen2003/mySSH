//! 设置应用层（M5）：settings KV → DOM/终端副作用。
//! 主题：根节点 data-ui（chrome 换肤）+ 全部已开终端的 xterm 调色板热切换。
//! 终端：字体族/字号热改 + fit 重算；回滚行数只影响新建终端（xterm 不支持在线改）。

import { fitRegistry, termRegistry } from '../term/registry';
import { resolveTheme } from '../term/themes';

export interface TerminalSettings {
  fontFamily: string;
  fontSize: number;
  scrollback: number;
}

export const DEFAULT_TERMINAL: TerminalSettings = {
  fontFamily: "'Cascadia Code', 'JetBrains Mono', Consolas, monospace",
  fontSize: 14,
  scrollback: 10_000,
};

/** 右键菜单外观默认值（px） */
export const DEFAULT_MENU_FONT = 12;
export const DEFAULT_MENU_ICON = 14;

const num = (v: unknown, dflt: number, min: number, max: number): number =>
  typeof v === 'number' && v >= min && v <= max ? v : dflt;
const str = (v: unknown, dflt: string): string =>
  typeof v === 'string' && v.trim() !== '' ? v : dflt;

export function readTerminalSettings(s: Record<string, unknown>): TerminalSettings {
  return {
    fontFamily: str(s['terminal.fontFamily'], DEFAULT_TERMINAL.fontFamily),
    fontSize: num(s['terminal.fontSize'], DEFAULT_TERMINAL.fontSize, 10, 24),
    scrollback: num(s['terminal.scrollback'], DEFAULT_TERMINAL.scrollback, 1_000, 100_000),
  };
}

/** 主题应用：root data-ui + 全终端调色板。返回生效主题 id（供面板显示）。 */
export function applyTheme(settings: Record<string, unknown>): string {
  const def = resolveTheme(
    str(settings['theme'], 'one-dark'),
    typeof settings['theme.customJson'] === 'string'
      ? (settings['theme.customJson'] as string)
      : undefined,
  );
  document.documentElement.dataset.ui = def.ui;
  for (const term of termRegistry.values()) {
    term.options.theme = def.xterm;
  }
  return def.id;
}

/** 字体/字号热改 + fit；回滚行数不动既有终端。 */
export function applyTerminalSettings(settings: Record<string, unknown>): void {
  const t = readTerminalSettings(settings);
  document.body.style.fontFamily = t.fontFamily;
  for (const [paneId, term] of termRegistry) {
    term.options.fontFamily = t.fontFamily;
    term.options.fontSize = t.fontSize;
    fitRegistry.get(paneId)?.();
  }
}

/** 右键菜单字号/图标尺寸：写根节点 CSS 变量，ContextMenu 经 var() 消费。 */
export function applyMenuSettings(settings: Record<string, unknown>): void {
  const font = num(settings['ui.menuFontSize'], DEFAULT_MENU_FONT, 11, 14);
  const icon = num(settings['ui.menuIconSize'], DEFAULT_MENU_ICON, 12, 18);
  document.documentElement.style.setProperty('--myssh-menu-font', `${font}px`);
  document.documentElement.style.setProperty('--myssh-menu-icon', `${icon}px`);
}
