import { useCallback } from 'react';
import { useAppStore } from '../state/app-store';
import { zhCN, type MsgKey } from './zh-CN';
import { enUS } from './en-US';

/** 界面语言；持久化于 settings KV ui.language（缺省 zh-CN） */
export type Language = 'zh-CN' | 'en-US';

const TABLES: Record<Language, Record<MsgKey, string>> = { 'zh-CN': zhCN, 'en-US': enUS };

/** 组件外（store/工具函数）取当前语言；语言切换后下次渲染自动生效 */
export function currentLanguage(settings: Record<string, unknown>): Language {
  return settings['ui.language'] === 'en-US' ? 'en-US' : 'zh-CN';
}

/** 纯函数翻译：{name} 占位插值；未知键回退键名（开发期可见，便于发现漏配） */
export function translate(
  lang: Language,
  key: MsgKey,
  vars?: Record<string, string | number>,
): string {
  let s: string = TABLES[lang][key] ?? zhCN[key] ?? key;
  if (vars) for (const [k, v] of Object.entries(vars)) s = s.replaceAll(`{${k}}`, String(v));
  return s;
}

/**
 * 组件内翻译钩子：t('sidebar.new', { name }) 用法。
 * 语言切换经 zustand settings 触发重渲染，无需额外事件。
 */
export function useT() {
  const lang = useAppStore((s) => currentLanguage(s.settings));
  return useCallback(
    (key: MsgKey, vars?: Record<string, string | number>) => translate(lang, key, vars),
    [lang],
  );
}

export type { MsgKey };
/** 组件外（zustand action / 工具函数）翻译：直接读当前 settings，不走 hook */
export function tNow(key: MsgKey, vars?: Record<string, string | number>): string {
  return translate(currentLanguage(useAppStore.getState().settings), key, vars);
}
