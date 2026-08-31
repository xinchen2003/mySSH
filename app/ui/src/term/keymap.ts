//! 快捷键注册表（M5）：动作 → 组合键，方案预设（default/vim/emacs）+ 自定义覆盖。
//! 约束：组合键必须带修饰键（终端占用裸键）；vim/emacs 方案只重排修饰组合，不引入和弦。
import { tNow, type MsgKey } from '../i18n';

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

/** 动作条目：label 用 getter 取值时翻译——模块加载期不冻结语言，消费方重渲染即读到新文案 */
const action = (a: Omit<KeyAction, 'label'>): KeyAction => ({
  ...a,
  get label() {
    return tNow(`state.key.${a.id}` as MsgKey);
  },
});

export const KEY_ACTIONS: KeyAction[] = [
  action({ id: 'copy', default: 'Ctrl+Shift+C', alias: 'Ctrl+Insert' }),
  action({ id: 'paste', default: 'Ctrl+Shift+V', alias: 'Shift+Insert' }),
  action({ id: 'palette', default: 'Ctrl+Shift+P' }),
  action({ id: 'search', default: 'Ctrl+Shift+F' }),
  action({ id: 'newTab', default: 'Ctrl+Shift+N' }),
  // 批次十一：Ctrl+Shift+T 让位给重开已关闭标签（浏览器/终端行业惯例），新建会话迁 Ctrl+Shift+N
  action({ id: 'reopenClosedTab', default: 'Ctrl+Shift+T' }),
  action({ id: 'closeTab', default: 'Ctrl+Shift+W' }),
  action({ id: 'nextTab', default: 'Ctrl+Tab', vim: 'Alt+L', emacs: 'Ctrl+PageDown' }),
  action({ id: 'prevTab', default: 'Ctrl+Shift+Tab', vim: 'Alt+H', emacs: 'Ctrl+PageUp' }),
  action({ id: 'sftp', default: 'Ctrl+Shift+E' }),
  action({ id: 'metrics', default: 'Ctrl+Shift+M' }),
  action({ id: 'tunnels', default: 'Ctrl+Shift+U' }),
  action({ id: 'settings', default: 'Ctrl+,' }),
  action({ id: 'splitRow', default: 'Ctrl+Shift+ArrowRight', vim: 'Alt+V' }),
  action({ id: 'splitCol', default: 'Ctrl+Shift+ArrowDown', vim: 'Alt+S' }),
  // 批次十一：字号缩放 / 窗格焦点切换
  action({ id: 'zoomIn', default: 'Ctrl+=' }),
  action({ id: 'zoomOut', default: 'Ctrl+-' }),
  action({ id: 'resetZoom', default: 'Ctrl+0' }),
  action({ id: 'nextPane', default: 'Ctrl+Alt+ArrowRight' }),
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
