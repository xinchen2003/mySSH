//! 通知动作注册表（12.1）：通知只存可序列化的 actionId + arg，
//! 点击时经此表分发到真实实现。新增动作在此登记，不在 Notice 里塞函数。

import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from './app-store';

export const NOTICE_ACTIONS: Record<string, (arg?: string) => void> = {
  /** 资源管理器定位文件/目录（config_export 等产物路径） */
  'open-in-explorer': (arg) => {
    if (!arg) return;
    invoke('open_in_explorer', { path: arg }).catch((e: unknown) => {
      useAppStore.getState().notify(`打开目录失败: ${String(e)}`, 'error');
    });
  },
};

export function runNoticeAction(actionId: string, arg?: string): void {
  const fn = NOTICE_ACTIONS[actionId];
  if (fn) fn(arg);
  else useAppStore.getState().notify(`未知通知动作: ${actionId}`, 'warning');
}
