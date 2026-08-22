import type { Terminal } from '@xterm/xterm';

/** tab 本地 id → xterm 实例（dev 冒烟钩子用；生产同样注册，开销可忽略） */
export const termRegistry = new Map<string, Terminal>();
