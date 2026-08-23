import { useAppStore, type NotificationLevel } from '../state/app-store';

/** 各级别视觉样式（语义色：成功绿 / 信息中性 / 警告黄 / 错误红） */
const NOTICE_STYLE: Record<NotificationLevel, string> = {
  success: 'bg-green-700 text-white',
  info: 'bg-neutral-700 text-neutral-100',
  warning: 'bg-yellow-600 text-neutral-950',
  error: 'bg-red-700 text-white',
};

/**
 * 分级通知堆叠（批次一 7.7）：底部居中多条堆叠；
 * success/info/warning 自动消失（时长见 store NOTICE_TTL），error 常驻手动关。
 */
export function Notices() {
  const notices = useAppStore((s) => s.notices);
  const dismissNotice = useAppStore((s) => s.dismissNotice);
  if (notices.length === 0) return null;
  return (
    <div className="fixed bottom-10 left-1/2 z-40 flex w-96 max-w-[90vw] -translate-x-1/2 flex-col gap-1.5">
      {notices.map((n) => (
        <div
          key={n.id}
          role={n.level === 'error' ? 'alert' : 'status'}
          aria-live={n.level === 'error' ? 'assertive' : 'polite'}
          className={`flex items-start gap-2 rounded px-3 py-2 text-xs shadow-lg ${NOTICE_STYLE[n.level]}`}
        >
          <span className="min-w-0 flex-1 break-words">{n.message}</span>
          {n.level === 'error' && (
            <button
              className="shrink-0 rounded px-1 leading-tight hover:bg-black/20"
              onClick={() => dismissNotice(n.id)}
              aria-label="关闭通知"
            >
              ×
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
