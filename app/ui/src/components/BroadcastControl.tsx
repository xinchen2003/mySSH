import { useEffect } from 'react';
import { initBroadcastReceiver } from '../term/broadcast';
import { useAppStore } from '../state/app-store';

/**
 * 广播输入开关（条目 11）：开启后本窗口全部已连接 pane 同步收到同一输入，
 * 并经 Tauri event（myssh://broadcast-input）同步到其它 detached 窗口。
 * 挂载即注册跨窗口接收端（幂等；detached 子窗口跑同一份 App，同样生效）。
 * 样式对齐 header 工具栏按钮；开启态高亮并带「广播中」指示。
 */
export function BroadcastControl() {
  const enabled = useAppStore((s) => s.broadcastEnabled);
  const toggle = useAppStore((s) => s.toggleBroadcast);

  useEffect(() => {
    void initBroadcastReceiver();
  }, []);

  return (
    <button
      className={`flex items-center gap-1 rounded px-1 ${
        enabled ? 'bg-blue-600/40 text-blue-300 hover:bg-blue-600/50' : 'hover:bg-neutral-800'
      }`}
      onClick={toggle}
      aria-pressed={enabled}
      title={
        enabled
          ? '关闭广播输入（停止向所有窗格同步输入）'
          : '开启广播输入（所有已连接窗格同步输入）'
      }
    >
      ⇶{enabled && <span className="text-blue-300">广播中</span>}
    </button>
  );
}
