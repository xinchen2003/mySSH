import { Dialog } from './Dialog';
import { useT } from '../i18n';
/**
 * 危险操作确认弹窗（删除服务器 / SFTP 远程删除 / 关闭活跃标签共用）。
 *
 * 语义（批次二十二调整）：初始焦点在「确认」——Enter 即确认（enterAction 兜底
 * 焦点不在控件上的情形）；Esc/背景点击 = 取消；焦点陷阱/焦点恢复由 Dialog 基座保证。
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
  const t = useT();
  // Enter = 确认：确认按钮拿初始焦点（原生 Enter 即触发），enterAction 兜底
  // 焦点不在控件上的情形。Esc/背景点击仍 = 取消
  return (
    <Dialog
      title={title}
      onClose={onCancel}
      enterAction={onConfirm}
      panelClass="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
    >
      <h2 className="mb-2 text-base font-semibold text-neutral-100">{title}</h2>
      <div className="mb-4 text-xs leading-5 text-neutral-400">{children}</div>
      <div className="flex justify-end gap-2">
        <button
          type="button"
          className="rounded px-3 py-1 text-neutral-300 hover:bg-neutral-800"
          onClick={onCancel}
        >
          {t('dialogs.cancel')}
        </button>
        <button
          data-autofocus
          type="button"
          className="rounded bg-red-600 px-3 py-1 text-white hover:bg-red-500"
          onClick={onConfirm}
        >
          {confirmLabel}
        </button>
      </div>
    </Dialog>
  );
}
