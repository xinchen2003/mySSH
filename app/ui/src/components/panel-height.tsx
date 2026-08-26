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
        const next = Math.min(
          PANEL_MAX_H,
          Math.max(PANEL_MIN_H, d.startH + (d.startY - ev.clientY)),
        );
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
export const PANEL_MIN_W = 320;
export const PANEL_MAX_W = 960;

/** 宽度夹取：PANEL_MIN_W..PANEL_MAX_W，且不超出视口（留 32px 边距） */
function clampW(w: number): number {
  return Math.min(
    PANEL_MAX_W,
    Math.max(PANEL_MIN_W, w),
    Math.max(PANEL_MIN_W, window.innerWidth - 32),
  );
}

/** 弹层/抽屉拖拽调宽（批次十 4）：左缘 4px 拖拽条，向左拖变宽。
 *  拖拽中只改本地 state，pointerup 才写 settings KV（与 usePanelHeight 同约）。
 *  上限同时受 PANEL_MAX_W 与视口（留 32px 边距）约束。 */
export function usePanelWidth(settingKey: string, defaultW: number) {
  const saved = useAppStore((s) => s.settings[settingKey]);
  const setSetting = useAppStore((s) => s.setSetting);
  const [width, setWidth] = useState(typeof saved === 'number' ? clampW(saved) : defaultW);
  const dragRef = useRef<{ startX: number; startW: number } | null>(null);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      dragRef.current = { startX: e.clientX, startW: width };
      const move = (ev: PointerEvent) => {
        const d = dragRef.current;
        if (!d) return;
        // 拖拽条在面板左缘：向左拖 = 变宽
        setWidth(clampW(d.startW + (d.startX - ev.clientX)));
      };
      const up = (ev: PointerEvent) => {
        window.removeEventListener('pointermove', move);
        window.removeEventListener('pointerup', up);
        const d = dragRef.current;
        dragRef.current = null;
        if (!d) return;
        const next = clampW(d.startW + (d.startX - ev.clientX));
        setWidth(next);
        setSetting(settingKey, next);
      };
      window.addEventListener('pointermove', move);
      window.addEventListener('pointerup', up);
    },
    [width, settingKey, setSetting],
  );

  const widthHandle = (
    <div
      className="absolute inset-y-0 left-0 z-10 w-1 cursor-col-resize hover:bg-blue-700"
      onPointerDown={onPointerDown}
      title="拖拽调整面板宽度"
    />
  );

  return { width, widthHandle };
}
