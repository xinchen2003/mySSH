import { emit, listen } from '@tauri-apps/api/event';
import { useAppStore, windowLabel } from '../state/app-store';
import { paneIds } from './layout';

/**
 * 广播输入（条目 11）：开启后输入帧同步写入本窗口全部 connected pane，
 * 并经 Tauri event 转发到其它窗口（detached 子窗口各跑各的 App，互不可见对方 store）。
 *
 * 回环防护两层：
 * 1. emit 会投递回本窗口 → 帧带发送方窗口 label，接收端比对丢弃；
 * 2. 收到的事件只直写 session.write，不经 xterm onData → 不触发 inputHook，天然不再二次转发。
 */
export const BROADCAST_INPUT_EVENT = 'myssh://broadcast-input';

export interface BroadcastInputFrame {
  v: 1;
  /** 发送方窗口 label（回环防护用） */
  source: string;
  data: string;
}

/** 写入本窗口全部 connected pane；exclude 为发起 pane（其输入已由自身 onData 直发） */
export function writeLocalPanes(data: string, exclude?: { tabId: string; paneId: string }): void {
  const s = useAppStore.getState();
  for (const t of s.tabs)
    for (const pid of paneIds(t.layout)) {
      const p = t.panes[pid];
      if (!p || p.state !== 'connected') continue;
      if (exclude && t.id === exclude.tabId && pid === exclude.paneId) continue;
      p.session.write(data);
    }
}

/** 发送侧：本窗口直写扇出 + 跨窗口事件（调用方已判定广播开关开启） */
export function broadcastInput(fromTabId: string, fromPaneId: string, data: string): void {
  writeLocalPanes(data, { tabId: fromTabId, paneId: fromPaneId });
  void emit<BroadcastInputFrame>(BROADCAST_INPUT_EVENT, {
    v: 1,
    source: windowLabel,
    data,
  }).catch(() => undefined);
}

let receiverReady = false;

/** 接收侧注册（幂等；BroadcastControl 挂载时调用，detached 子窗口同样挂载同一份 App） */
export async function initBroadcastReceiver(): Promise<void> {
  if (receiverReady) return;
  receiverReady = true;
  try {
    await listen<BroadcastInputFrame>(BROADCAST_INPUT_EVENT, (e) => {
      if (e.payload.source === windowLabel) return; // 回环防护：emit 会回本窗口
      writeLocalPanes(e.payload.data);
    });
  } catch {
    receiverReady = false; // 非 Tauri 环境（vite 浏览器诊断）：允许重试
  }
}
