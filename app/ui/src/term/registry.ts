import type { Terminal } from '@xterm/xterm';

/** pane id → xterm 实例（dev 冒烟钩子用；生产同样注册，开销可忽略） */
export const termRegistry = new Map<string, Terminal>();

/** pane id → fit 回调（字体/字号设置变更后重算 cols/rows；0 尺寸时跳过由实现方保证） */
export const fitRegistry = new Map<string, () => void>();
