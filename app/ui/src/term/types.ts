/**
 * 前端类型入口。线型唯一源头 = Rust（ts-rs 生成于 ./bindings/，`cargo test` 时刷新，
 * 文件入库——diff 即漂移，勿手改）。本文件 = bindings re-export 收敛 + 纯前端内部类型。
 * 语义/时序/错误码说明见 docs/design/03-ipc-contract.md。
 */
export type { AuthSpec } from './bindings/AuthSpec';
export type { SessionRecord } from './bindings/SessionRecord';
export type { TunnelRecord as TunnelDef } from './bindings/TunnelRecord';
export type { TunnelInfo } from './bindings/TunnelInfo';
export type { SessionTunnelResult } from './bindings/SessionTunnelResult';
export type { FileEntry } from './bindings/FileEntry';
export type { TransferView } from './bindings/TransferView';
export type { TransferJobView } from './bindings/TransferJobView';
export type { TransferHistoryView } from './bindings/TransferHistoryView';
export type { TermOpenSpec } from './bindings/TermOpenSpec';
export type { MetricsSnapshot } from './bindings/MetricsSnapshot';
export type { MetricsEvent } from './bindings/MetricsEvent';
export type { TestConnectRequest } from './bindings/TestConnectRequest';

import type { TermOpenSpec } from './bindings/TermOpenSpec';
import type { HostKeyPromptFrame } from './bindings/HostKeyPromptFrame';
import type { KiChallengeFrame } from './bindings/KiChallengeFrame';
import type { SessionStateFrame } from './bindings/SessionStateFrame';
import type { SessionTunnelsFrame } from './bindings/SessionTunnelsFrame';
import type { MacroSkippedFrame } from './bindings/MacroSkippedFrame';

export type {
  HostKeyPromptFrame,
  KiChallengeFrame,
  SessionStateFrame,
  SessionTunnelsFrame,
  MacroSkippedFrame,
};

/** 隧道类型（与 core-store TunnelRecord.kind / TunnelInfo.kind 的值域一致） */
export type TunnelKind = 'local' | 'remote' | 'dynamic';

/** 隧道启动方式（前端归类概念：定义标记位的联合） */
export type TunnelStartMode = 'withSession' | 'autostart' | 'manual';

/** 连接目标：内联参数 或 存储档案引用。
 *  sessionKind 在建标签时从档案定死（ssh 缺省）——SSH 专属功能门控以此为准，
 *  不依赖 sessions 列表查表（档案未加载/已删除而标签还在时不误判）。 */
export type ConnectTarget =
  | { kind: 'spec'; spec: TermOpenSpec }
  | {
      kind: 'session';
      sessionId: string;
      sessionKind?: 'ssh' | 'local';
      /** 建档时的终端编码快照；后端缺省回退档案值 */
      encoding?: string | null;
    };

/** 终端事件帧（后端 wire 帧的判别联合，tag = type） */
export type TermEvent =
  | HostKeyPromptFrame
  | KiChallengeFrame
  | SessionStateFrame
  | SessionTunnelsFrame
  | MacroSkippedFrame;
