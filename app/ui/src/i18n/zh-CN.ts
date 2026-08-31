import { zhChrome } from './messages/chrome.zh-CN';
import { zhDialogs } from './messages/dialogs.zh-CN';
import { zhPanels } from './messages/panels.zh-CN';
import { zhState } from './messages/state.zh-CN';

/**
 * 中文（默认语言）文案表：按组切片合并（messages/*.zh-CN.ts）。
 * 键命名约定 <组>.<名>（chrome/dialogs/panels/state），动态值用 {name} 占位。
 * 新增键必须同步对应 .en-US.ts 切片（Record 类型强制）。
 */
export const zhCN = {
  'app.title': 'mySSH',
  ...zhChrome,
  ...zhDialogs,
  ...zhPanels,
  ...zhState,
} as const;

export type MsgKey = keyof typeof zhCN;
