/**
 * term_open 的认证材料（serde tag=type + camelCase，与 app crate AuthSpec 对齐）
 */
export type AuthSpec =
  | { type: 'password'; password: string }
  | { type: 'publicKey'; keyPem: string; passphrase?: string | null }
  | { type: 'keyboardInteractive' }
  | { type: 'agent' };

/** 与 core-store SessionRecord 对齐（camelCase 序列化） */
export interface SessionRecord {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authType: 'password' | 'publickey' | 'keyboard-interactive' | 'agent';
  keyPath?: string | null;
  groupPath: string;
  tags: string[];
  command?: string | null;
  createdAt: string;
  updatedAt: string;
}

export type TunnelKind = 'local' | 'remote' | 'dynamic';

export interface TunnelInfo {
  tunnelId: string;
  kind: TunnelKind;
  bind: string;
  target: string | null;
  status: 'starting' | 'listening' | 'reconnecting' | 'stopped' | 'failed';
  activeConns: number;
  totalConns: number;
  bytesUp: number;
  bytesDown: number;
  rateUp: number;
  rateDown: number;
  errors: number;
  reconnects: number;
}

export interface TunnelForm {
  sessionId: string;
  kind: TunnelKind;
  bindHost: string;
  bindPort: number;
  targetHost?: string;
  targetPort?: number;
}

/** 连接目标：内联参数 或 存储档案引用 */
export type ConnectTarget =
  { kind: 'spec'; spec: TermOpenSpec } | { kind: 'session'; sessionId: string };

export interface TermOpenSpec {
  host: string;
  port: number;
  user: string;
  auth: AuthSpec;
  /** 默认 xterm-256color */
  term?: string;
  /** 缺省为登录 shell */
  command?: string;
}

export interface HostKeyPromptFrame {
  v: 1;
  type: 'hostkey_prompt';
  confirmId: string;
  kind: 'unknown' | 'changed';
  host: string;
  port: number;
  keyType: string;
  fingerprint?: string;
  oldFingerprint?: string;
  newFingerprint?: string;
}

export interface KiChallengeFrame {
  v: 1;
  type: 'ki_challenge';
  confirmId: string;
  name: string;
  instruction: string;
  prompts: { prompt: string; echo: boolean }[];
}

export interface SessionStateFrame {
  v: 1;
  type: 'session_state';
  tabId: string;
  state: 'connected' | 'closed' | 'reconnecting' | 'error';
  message?: string;
  /** reconnecting 时的第几次尝试 */
  attempt?: number;
  /** true = 断线重连成功（区别于首次连接） */
  reconnected?: boolean;
}

export type TermEvent = HostKeyPromptFrame | KiChallengeFrame | SessionStateFrame;
