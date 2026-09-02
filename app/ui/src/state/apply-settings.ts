//! 设置应用层（M5）：settings KV → DOM/终端副作用。
//! 主题：根节点 data-ui（chrome 换肤）+ 全部已开终端的 xterm 调色板热切换。
//! 终端：字体族/字号热改 + fit 重算；回滚行数只影响新建终端（xterm 不支持在线改）。

import { fitRegistry, termRegistry, webglRegistry } from '../term/registry';
import type { ITheme } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { resolveTheme, type ThemeDef } from '../term/themes';
/** 背景图热更：CSS 变量写根节点，全部 pane 的背景层自动跟随。
 *  WebGL 画布不透明（实测）：有图时 dispose 已载 WebGL 回退内置渲染器；清图后重建恢复加速。 */
export function applyTerminalBackground(settings: Record<string, unknown>): void {
  const bg = readTermBackground(settings);
  const root = document.documentElement.style;
  root.setProperty('--myssh-term-bg-image', bg.image ? `url("${bg.image}")` : 'none');
  root.setProperty('--myssh-term-bg-opacity', String(bg.opacity));
  // 有图时视口放行：xterm.css 的 .xterm-viewport 默认黑底会盖住背景层
  document.documentElement.dataset.termBg = bg.image ? '1' : '';
  if (bg.image) {
    for (const [paneId, addon] of webglRegistry) {
      try {
        addon.dispose();
      } catch {
        // 已 dispose 的 addon 重复 dispose 抛错，忽略
      }
      webglRegistry.delete(paneId);
    }
  } else {
    for (const [paneId, term] of termRegistry) {
      if (webglRegistry.has(paneId)) continue;
      try {
        const addon = new WebglAddon();
        term.loadAddon(addon);
        webglRegistry.set(paneId, addon);
      } catch {
        // WebGL 不可用则保持内置渲染器
      }
    }
  }
}

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

/** 右键菜单外观固定值（px；图标较早期版本放大一倍） */
export const MENU_FONT = 12;
export const MENU_ICON = 28;

const num = (v: unknown, dflt: number, min: number, max: number): number =>
  typeof v === 'number' && v >= min && v <= max ? v : dflt;
const str = (v: unknown, dflt: string): string =>
  typeof v === 'string' && v.trim() !== '' ? v : dflt;

export function readTerminalSettings(s: Record<string, unknown>): TerminalSettings {
  return {
    fontFamily: str(s['terminal.fontFamily'], DEFAULT_TERMINAL.fontFamily),
    fontSize: num(s['terminal.fontSize'], DEFAULT_TERMINAL.fontSize, 8, 32),
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
    term.options.theme = effectiveXtermTheme(settings, def);
  }
  return def.id;
}
/** 终端背景图设置：image 为 data URL（空 = 无图），opacity 0.05-1 */
export interface TermBackground {
  image: string;
  opacity: number;
}

export function readTermBackground(s: Record<string, unknown>): TermBackground {
  return {
    image: str(s['terminal.backgroundImage'], ''),
    opacity: num(s['terminal.backgroundOpacity'], 0.35, 0.05, 1),
  };
}

/** 有背景图时 xterm 背景透明化（需构造时 allowTransparency），否则用主题原色 */
export function effectiveXtermTheme(settings: Record<string, unknown>, def: ThemeDef): ITheme {
  if (!readTermBackground(settings).image) return def.xterm;
  return { ...def.xterm, background: 'transparent' };
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

/** 右键菜单字号/图标尺寸：固定值写根节点 CSS 变量，ContextMenu 经 var() 消费。 */
export function applyMenuSettings(): void {
  document.documentElement.style.setProperty('--myssh-menu-font', `${MENU_FONT}px`);
  document.documentElement.style.setProperty('--myssh-menu-icon', `${MENU_ICON}px`);
}
