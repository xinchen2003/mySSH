//! 快捷键注册表（M5）：动作 → 组合键，方案预设（default/vim/emacs）+ 自定义覆盖。
//! 约束：组合键必须带修饰键（终端占用裸键）；vim/emacs 方案只重排修饰组合，不引入和弦。

export interface KeyAction {
  id: string;
  label: string;
  /** default 方案的绑定（vim/emacs 无覆盖时回落 default） */
  default: string;
  vim?: string;
  emacs?: string;
  /** 固定别名组合（终端行业惯例，不可改不随方案；设置/命令面板随主绑定一并展示） */
  alias?: string;
}

export const KEY_ACTIONS: KeyAction[] = [
  { id: 'copy', label: '复制选中', default: 'Ctrl+Shift+C', alias: 'Ctrl+Insert' },
  { id: 'paste', label: '粘贴', default: 'Ctrl+Shift+V', alias: 'Shift+Insert' },
  { id: 'palette', label: '命令面板', default: 'Ctrl+Shift+P' },
  { id: 'search', label: '终端内搜索', default: 'Ctrl+Shift+F' },
  { id: 'newTab', label: '新建会话', default: 'Ctrl+Shift+N' },
  // 批次十一：Ctrl+Shift+T 让位给重开已关闭标签（浏览器/终端行业惯例），新建会话迁 Ctrl+Shift+N
  { id: 'reopenClosedTab', label: '重开已关闭标签', default: 'Ctrl+Shift+T' },
  { id: 'closeTab', label: '关闭标签', default: 'Ctrl+Shift+W' },
  {
    id: 'nextTab',
    label: '下一标签',
    default: 'Ctrl+Tab',
    vim: 'Alt+L',
    emacs: 'Ctrl+PageDown',
  },
  {
    id: 'prevTab',
    label: '上一标签',
    default: 'Ctrl+Shift+Tab',
    vim: 'Alt+H',
    emacs: 'Ctrl+PageUp',
  },
  { id: 'sftp', label: 'SFTP 面板', default: 'Ctrl+Shift+E' },
  { id: 'metrics', label: '监控面板', default: 'Ctrl+Shift+M' },
  { id: 'tunnels', label: '隧道面板', default: 'Ctrl+Shift+U' },
  { id: 'settings', label: '设置', default: 'Ctrl+,' },
  {
    id: 'splitRow',
    label: '向右分屏',
    default: 'Ctrl+Shift+ArrowRight',
    vim: 'Alt+V',
  },
  {
    id: 'splitCol',
    label: '向下分屏',
    default: 'Ctrl+Shift+ArrowDown',
    vim: 'Alt+S',
  },
  // 批次十一：字号缩放 / 窗格焦点切换
  { id: 'zoomIn', label: '放大字号', default: 'Ctrl+=' },
  { id: 'zoomOut', label: '缩小字号', default: 'Ctrl+-' },
  { id: 'resetZoom', label: '重置字号', default: 'Ctrl+0' },
  { id: 'nextPane', label: '下一窗格', default: 'Ctrl+Alt+ArrowRight' },
];

export type KeymapScheme = 'default' | 'vim' | 'emacs';

/** 解析生效绑定：scheme 覆盖 → custom 覆盖 → default */
export function effectiveBindings(
  scheme: KeymapScheme,
  custom?: Record<string, string>,
): Record<string, string> {
  const out: Record<string, string> = {};
  for (const a of KEY_ACTIONS) {
    out[a.id] = (scheme !== 'default' && a[scheme]) || a.default;
  }
  if (custom) {
    for (const [id, combo] of Object.entries(custom)) {
      if (id in out && typeof combo === 'string' && isValidCombo(combo)) out[id] = combo;
    }
  }
  return out;
}

/** 组合键合法性：必须含至少一个修饰键（防占终端裸键），格式 Mod+Mod+Key */
export function isValidCombo(combo: string): boolean {
  const parts = combo.split('+').map((p) => p.trim());
  if (parts.length < 2) return false;
  const mods = parts.slice(0, -1);
  const key = parts[parts.length - 1];
  return (
    mods.every((m) => ['Ctrl', 'Alt', 'Shift'].includes(m)) &&
    key.length > 0 &&
    !['Ctrl', 'Alt', 'Shift'].includes(key)
  );
}

/** KeyboardEvent 匹配组合键（key 比较大小写不敏感） */
export function matchCombo(e: KeyboardEvent, combo: string): boolean {
  const parts = combo.split('+').map((p) => p.trim());
  const key = parts[parts.length - 1];
  const wantCtrl = parts.includes('Ctrl');
  const wantAlt = parts.includes('Alt');
  const wantShift = parts.includes('Shift');
  if (e.ctrlKey !== wantCtrl || e.altKey !== wantAlt || e.shiftKey !== wantShift) return false;
  // 特殊键用 e.code 语义名（ArrowRight/PageDown/Tab/Insert），字符键大小写不敏感
  if (/^(Arrow|Page|Tab|Enter|Escape|Home|End|Space|Insert|Delete)/.test(key)) {
    return e.code === key || e.key === key;
  }
  return e.key.toLowerCase() === key.toLowerCase() || e.code === `Key${key.toUpperCase()}`;
}

/** 动作匹配：主绑定（可自定义）或固定别名其一命中即算 */
export function matchAction(
  e: KeyboardEvent,
  bindings: Record<string, string>,
  id: string,
): boolean {
  if (bindings[id] && matchCombo(e, bindings[id])) return true;
  const alias = KEY_ACTIONS.find((a) => a.id === id)?.alias;
  return !!alias && matchCombo(e, alias);
}

/** 从设置读键位方案 */
export function keymapFromSettings(s: Record<string, unknown>): Record<string, string> {
  const schemeRaw = s['keymap.scheme'];
  const scheme: KeymapScheme = schemeRaw === 'vim' || schemeRaw === 'emacs' ? schemeRaw : 'default';
  return effectiveBindings(scheme);
}
