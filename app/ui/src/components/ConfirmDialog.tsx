import { useEffect, useRef } from 'react';

/**
 * 危险操作确认弹窗（删除服务器 / SFTP 远程删除 / 关闭活跃标签共用）。
 *
 * 安全语义：
 * - 初始焦点在「取消」——打开瞬间误按 Enter 触发的是取消而非危险操作；
 * - Esc 等价于取消；
 * - backdrop 点击取消；
 * - 确认按钮固定危险红样式。
 * 焦点陷阱等通用对话框基座能力归批次四统一抽取，此处保持最小实现。
 */
export function ConfirmDialog({
  title,
  children,
  confirmLabel,
  onConfirm,
  onCancel,
}: {
  title: string;
  children: React.ReactNode;
  confirmLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    cancelRef.current?.focus();
    // capture 阶段拦截 Esc，阻止落入终端/全局快捷键
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation();
        e.preventDefault();
        onCancel();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [onCancel]);

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      role="dialog"
      aria-modal="true"
      aria-label={title}
      onClick={onCancel}
    >
      <div
        className="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="mb-2 text-base font-semibold text-neutral-100">{title}</h2>
        <div className="mb-4 text-xs leading-5 text-neutral-400">{children}</div>
        <div className="flex justify-end gap-2">
          <button
            ref={cancelRef}
            type="button"
            className="rounded px-3 py-1 text-neutral-300 hover:bg-neutral-800"
            onClick={onCancel}
          >
            取消
          </button>
          <button
            type="button"
            className="rounded bg-red-600 px-3 py-1 text-white hover:bg-red-500"
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
