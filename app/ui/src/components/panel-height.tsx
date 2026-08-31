import { useCallback, useRef, useState } from 'react';
import { useAppStore } from '../state/app-store';

/** 面板拖拽调高（批次五 11.5）：顶边 4px 拖拽条，pointer 事件驱动（不轮询），
 *  拖拽中只改本地 state，pointerup 才写 settings KV（避免高频持久化）。 */

export const PANEL_MIN_H = 140;
export const PANEL_MAX_H = 600;

/** 高度夹取：PANEL_MIN_H..PANEL_MAX_H（拖拽与键盘调幅共用） */
function clampH(h: number): number {
  return Math.min(PANEL_MAX_H, Math.max(PANEL_MIN_H, h));
}

export function usePanelHeight(settingKey: string, defaultH: number) {
  const saved = useAppStore((s) => s.settings[settingKey]);
  const setSetting = useAppStore((s) => s.setSetting);
  const [height, setHeight] = useState(
    typeof saved === 'number' ? Math.min(PANEL_MAX_H, Math.max(PANEL_MIN_H, saved)) : defaultH,
  );
  const dragRef = useRef<{ startY: number; startH: number } | null>(null);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      dragRef.current = { startY: e.clientY, startH: height };
      const move = (ev: PointerEvent) => {
        const d = dragRef.current;
        if (!d) return;
        // 拖拽条在面板顶边：向上拖 = 变高
        const next = d.startH + (d.startY - ev.clientY);
        setHeight(clampH(next));
      };
      const up = (ev: PointerEvent) => {
        window.removeEventListener('pointermove', move);
        window.removeEventListener('pointerup', up);
        const d = dragRef.current;
        dragRef.current = null;
        if (!d) return;
        const next = clampH(d.startH + (d.startY - ev.clientY));
        setHeight(next);
        setSetting(settingKey, next);
      };
      window.addEventListener('pointermove', move);
      window.addEventListener('pointerup', up);
    },
    [height, settingKey, setSetting],
  );

  // 键盘调幅（与拖拽同 clamp）：↑ 增高 / ↓ 降低，步长 16px（约 2 行终端文本）
  const onKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      const delta = e.key === 'ArrowUp' ? 16 : e.key === 'ArrowDown' ? -16 : 0;
      if (!delta) return;
      e.preventDefault();
      const next = clampH(height + delta);
      setHeight(next);
      setSetting(settingKey, next);
    },
    [height, settingKey, setSetting],
  );

  const handle = (
    <div
      role="separator"
      tabIndex={0}
      aria-orientation="horizontal"
      aria-label="调整面板高度"
      className="h-1 shrink-0 cursor-row-resize bg-neutral-800 hover:bg-blue-700 focus-visible:ring-1 focus-visible:ring-neutral-500"
      onPointerDown={onPointerDown}
      onKeyDown={onKeyDown}
      title="拖拽调整面板高度"
    />
  );

  return { height, handle };
}
