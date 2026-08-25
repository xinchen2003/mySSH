import { useEffect, useRef } from 'react';

/**
 * 统一对话框基座（批次四 10.5）。
 *
 * 行为契约：
 * - Esc 关闭（capture 阶段拦截，阻止落入终端/全局快捷键；closeOnEscape 可关）；
 * - 焦点陷阱：Tab/Shift+Tab 只在面板内循环；
 * - 初始焦点：`[data-autofocus]` 标记的元素优先，其次 initialFocus 选择器，
 *   都没有则聚焦面板内第一个可交互元素；危险操作由调用方把 data-autofocus 放在「取消」上；
 * - 关闭后焦点恢复到打开前的元素；
 * - backdrop 点击关闭可配置（closeOnBackdrop）；
 * - aria：role=dialog + aria-modal + aria-label；
 * - Enter 默认操作可配置（enterAction，焦点在输入控件/按钮上时不劫持，走原生行为）。
 *
 * 不在内：复杂自有键盘模型的浮层（命令面板）不强制使用本基座。
 */
export function Dialog({
  title,
  onClose,
  children,
  backdropClass,
  panelClass,
  closeOnBackdrop = true,
  closeOnEscape = true,
  initialFocus,
  enterAction,
}: {
  title: string;
  onClose: () => void;
  children: React.ReactNode;
  /** 遮罩层附加 class（z-index/对齐差异由此传入） */
  backdropClass?: string;
  /** 面板 class（尺寸/配色由调用方定） */
  panelClass?: string;
  closeOnBackdrop?: boolean;
  closeOnEscape?: boolean;
  /** 面板内选择器，优先于 data-autofocus 之外的默认焦点 */
  initialFocus?: string;
  /** Enter 默认操作（焦点在 button/input/select/textarea/a 上时不触发） */
  enterAction?: () => void;
}) {
  const panelRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const panel = panelRef.current;
    if (!panel) return;
    const prev = document.activeElement instanceof HTMLElement ? document.activeElement : null;

    const focusables = () =>
      Array.from(
        panel.querySelectorAll<HTMLElement>(
          'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((el) => el.offsetParent !== null);

    // 初始焦点：data-autofocus > initialFocus 选择器 > 首个可交互元素
    const marked = panel.querySelector<HTMLElement>('[data-autofocus]');
    const named = initialFocus ? panel.querySelector<HTMLElement>(initialFocus) : null;
    (marked ?? named ?? focusables()[0] ?? panel).focus();

    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        if (!closeOnEscape) return;
        e.stopPropagation();
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key === 'Tab') {
        const list = focusables();
        if (list.length === 0) {
          e.preventDefault();
          return;
        }
        const idx = list.indexOf(document.activeElement as HTMLElement);
        const step = e.shiftKey ? -1 : 1;
        const next = list[(idx + step + list.length) % list.length];
        e.preventDefault();
        e.stopPropagation();
        next.focus();
        return;
      }
      if (e.key === 'Enter' && enterAction) {
        const t = e.target as HTMLElement | null;
        if (t && t.closest('button, input, select, textarea, a')) return;
        e.preventDefault();
        enterAction();
      }
    };
    window.addEventListener('keydown', onKey, true);
    return () => {
      window.removeEventListener('keydown', onKey, true);
      // 焦点恢复：目标可能已卸载，静默忽略
      prev?.focus();
    };
    // onClose/enterAction 取挂载时版本即可：对话框内容在生命周期内不换回调语义
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      className={`fixed inset-0 z-50 flex items-center justify-center bg-black/60 ${backdropClass ?? ''}`}
      onClick={closeOnBackdrop ? onClose : undefined}
    >
      <div
        ref={panelRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        tabIndex={-1}
        className={panelClass}
        onClick={(e) => e.stopPropagation()}
      >
        {children}
      </div>
    </div>
  );
}
