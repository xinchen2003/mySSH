import type { MsgKey } from './zh-CN';
import { enChrome } from './messages/chrome.en-US';
import { enDialogs } from './messages/dialogs.en-US';
import { enPanels } from './messages/panels.en-US';
import { enState } from './messages/state.en-US';

/** 英文文案表：键集合必须与 zh-CN 完全一致（类型层面强制）。 */
export const enUS: Record<MsgKey, string> = {
  'app.title': 'mySSH',
  ...enChrome,
  ...enDialogs,
  ...enPanels,
  ...enState,
};
