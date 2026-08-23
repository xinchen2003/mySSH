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
  /** ProxyJump 链：session id 数组（就近→最远）；空 = 直连 */
  jumpChain: string[];
  groupPath: string;
  tags: string[];
  command?: string | null;
  createdAt: string;
  updatedAt: string;
}

export type TunnelKind = 'local' | 'remote' | 'dynamic';

/** 持久化隧道定义（core-store TunnelRecord 对齐） */
export interface TunnelDef {
  id: string;
  sessionId: string;
  kind: TunnelKind;
  bindHost: string;
  bindPort: number;
  targetHost?: string | null;
  targetPort?: number | null;
  /** 开机自启（app 启动即建立） */
  autostart: boolean;
  /** 随会话自动建立 */
  withSession: boolean;
  createdAt: string;
}

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

/** SFTP/本地目录条目（app sftp_list/local_list 对齐） */
export interface FileEntry {
  name: string;
  path: string;
  kind: 'file' | 'dir' | 'symlink' | 'other';
  size: number;
  permissions?: number | null;
  mtime?: number | null;
  user?: string | null;
  group?: string | null;
}

/** 传输快照（app transfer_subscribe 帧对齐；rate 由后端差分） */
export interface TransferView {
  id: string;
  direction: 'upload' | 'download';
  local: string;
  remote: string;
  state: 'queued' | 'running' | 'paused' | 'done' | 'failed' | 'canceled';
  bytesDone: number;
  bytesTotal: number;
  retries: number;
  error?: string | null;
  rate?: number;
  /** 历史记录（上次运行终态，非本次运行） */
  history?: boolean;
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

/** 监控快照（core-monitor camelCase 直推） */
export interface MetricsSnapshot {
  tsMs: number;
  intervalMs: number;
  cpuBusyPct?: number | null;
  load: [number, number, number];
  procsRunning: number;
  procsTotal: number;
  memTotalKb: number;
  memAvailKb: number;
  swapTotalKb: number;
  swapFreeKb: number;
  disks: { name: string; readBps?: number | null; writeBps?: number | null }[];
  nets: { iface: string; rxBps?: number | null; txBps?: number | null }[];
  procs: { pid: number; rssKb: number; cpuPct: number; memPct: number; comm: string }[];
}

/** metrics_subscribe 推送帧 */
export type MetricsEvent =
  { kind: 'snapshot'; data: MetricsSnapshot } | { kind: 'error'; message: string; fatal: boolean };
