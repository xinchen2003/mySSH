/**
 * term_open 的认证材料（serde tag=type + camelCase，与 app crate AuthSpec 对齐）
 */
export type AuthSpec =
  | { type: 'password'; password: string }
  | { type: 'publicKey'; keyPem: string; passphrase?: string | null }
  | { type: 'keyboardInteractive' }
  | { type: 'agent' };

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
