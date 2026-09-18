/**
 * 多行粘贴判定（批次十一安全确认）：快捷键/右键直贴/菜单项三路共用。
 *
 * 规则：去除尾部单个换行（复制时常见的尾换行）后仍含换行 → 需确认框；
 * 否则直贴。确认框默认焦点在「取消」，防误回车批量执行命令。
 */
export function pasteNeedsConfirm(text: string): { lines: number } | null {
  const stripped = text.replace(/\r?\n$/, '');
  if (!stripped.includes('\n')) return null;
  return { lines: stripped.split('\n').length };
}
