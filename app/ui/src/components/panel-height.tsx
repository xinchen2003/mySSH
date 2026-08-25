import { useCallback, useRef, useState } from 'react';
import { useAppStore } from '../state/app-store';

/** 面板拖拽调高（批次五 11.5）：顶边 4px 拖拽条，pointer 事件驱动（不轮询），
 *  拖拽中只改本地 state，pointerup 才写 settings KV（避免高频持久化）。 */

export const PANEL_MIN_H = 140;
export const PANEL_MAX_H = 600;

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
        setHeight(Math.min(PANEL_MAX_H, Math.max(PANEL_MIN_H, next)));
      };
      const up = (ev: PointerEvent) => {
        window.removeEventListener('pointermove', move);
        window.removeEventListener('pointerup', up);
        const d = dragRef.current;
        dragRef.current = null;
        if (!d) return;
        const next = Math.min(PANEL_MAX_H, Math.max(PANEL_MIN_H, d.startH + (d.startY - ev.clientY)));
        setHeight(next);
        setSetting(settingKey, next);
      };
      window.addEventListener('pointermove', move);
      window.addEventListener('pointerup', up);
    },
    [height, settingKey, setSetting],
  );

  const handle = (
    <div
      className="h-1 shrink-0 cursor-row-resize bg-neutral-800 hover:bg-blue-700"
      onPointerDown={onPointerDown}
      title="拖拽调整面板高度"
    />
  );

  return { height, handle };
}
