//! 隧道纯逻辑（§9）：启动方式单选映射、模板表、草稿校验、速率格式化。
//! 与渲染解耦，vitest 直测。

import type { TunnelDef, TunnelKind, TunnelStartMode } from '../term/types';
import { tNow } from '../i18n';

/** 启动方式 ← 两个持久化布尔位。旧数据双真（自启+随会话）归并为「随服务器连接」：
 *  编辑保存后落为 with_session=1/autostart=0；未编辑的旧记录行为不变（后端仍按原位消费）。 */
export function startModeOf(def: Pick<TunnelDef, 'autostart' | 'withSession'>): TunnelStartMode {
  if (def.withSession) return 'withSession';
  if (def.autostart) return 'autostart';
  return 'manual';
}

/** 启动方式 → 布尔位（单选互斥） */
export function startModeFlags(mode: TunnelStartMode): {
  autostart: boolean;
  withSession: boolean;
} {
  return {
    autostart: mode === 'autostart',
    withSession: mode === 'withSession',
  };
}

export const START_MODE_LABEL: Record<TunnelStartMode, string> = {
  // getter 取值时翻译：语言切换后消费方重渲染即读到新文案（键名经 tNow 查表）
  get withSession() {
    return tNow('state.startMode.withSession');
  },
  get autostart() {
    return tNow('state.startMode.autostart');
  },
  get manual() {
    return tNow('state.startMode.manual');
  },
};

/** §9.5 模板：只预填名称/类型/端口，不隐藏实际配置 */
export interface TunnelTemplate {
  label: string;
  kind: TunnelKind;
  bindPort: number;
  targetHost?: string;
  targetPort?: number;
}

export const TUNNEL_TEMPLATES: Record<string, TunnelTemplate> = {
  mysql: {
    label: 'MySQL',
    kind: 'local',
    bindPort: 3306,
    targetHost: '127.0.0.1',
    targetPort: 3306,
  },
  postgres: {
    label: 'PostgreSQL',
    kind: 'local',
    bindPort: 5432,
    targetHost: '127.0.0.1',
    targetPort: 5432,
  },
  redis: {
    label: 'Redis',
    kind: 'local',
    bindPort: 6379,
    targetHost: '127.0.0.1',
    targetPort: 6379,
  },
  rdp: { label: 'RDP', kind: 'local', bindPort: 3389, targetHost: '127.0.0.1', targetPort: 3389 },
  http: { label: 'HTTP', kind: 'local', bindPort: 8080, targetHost: '127.0.0.1', targetPort: 80 },
  socks5: { label: 'SOCKS5', kind: 'dynamic', bindPort: 1080 },
};

/** 编辑器草稿（string 端口以承载输入中间态） */
export interface TunnelDraft {
  id: string;
  sessionId: string;
  name: string;
  kind: TunnelKind;
  bindHost: string;
  bindPort: string;
  targetHost: string;
  targetPort: string;
  startMode: TunnelStartMode;
}

export function draftFromDef(def: TunnelDef): TunnelDraft {
  return {
    id: def.id,
    sessionId: def.sessionId,
    name: def.name,
    kind: def.kind,
    bindHost: def.bindHost,
    bindPort: String(def.bindPort),
    targetHost: def.targetHost ?? '',
    targetPort: def.targetPort ? String(def.targetPort) : '',
    startMode: startModeOf(def),
  };
}

/** 校验草稿；返回第一条错误文案，合法返回 null（与后端 validate 口径一致） */
export function validateTunnelDraft(d: TunnelDraft): string | null {
  if (!d.bindHost.trim()) return tNow('state.errBindHostRequired');
  const bp = Number(d.bindPort);
  if (!Number.isInteger(bp) || bp < 1 || bp > 65535) return tNow('state.errBindPortRange');
  if (d.kind !== 'dynamic') {
    if (!d.targetHost.trim()) return tNow('state.errTargetRequired');
    const tp = Number(d.targetPort);
    if (!Number.isInteger(tp) || tp < 1 || tp > 65535) return tNow('state.errTargetPortRange');
  }
  return null;
}

/** 草稿 → 定义（调用方保证已通过 validateTunnelDraft） */
export function draftToDef(d: TunnelDraft, createdAt: string): TunnelDef {
  const flags = startModeFlags(d.startMode);
  return {
    id: d.id,
    sessionId: d.sessionId,
    name: d.name.trim(),
    kind: d.kind,
    bindHost: d.bindHost.trim(),
    bindPort: Number(d.bindPort),
    targetHost: d.kind === 'dynamic' ? null : d.targetHost.trim(),
    targetPort: d.kind === 'dynamic' ? null : Number(d.targetPort),
    autostart: flags.autostart,
    withSession: flags.withSession,
    createdAt,
  };
}

/** 通配绑定（0.0.0.0 / :: / 空）需要安全警告 */
export function isWildcardBind(host: string): boolean {
  const h = host.trim();
  return h === '0.0.0.0' || h === '::' || h === '*';
}

/** 速率数字部分格式化（模块级缓存；观感同 toFixed(1)） */
const rateNum = new Intl.NumberFormat('en-US', {
  minimumFractionDigits: 1,
  maximumFractionDigits: 1,
});

export function fmtRate(bytesPerSec: number): string {
  if (bytesPerSec >= 1 << 20) return `${rateNum.format(bytesPerSec / (1 << 20))} MB/s`;
  if (bytesPerSec >= 1024) return `${rateNum.format(bytesPerSec / 1024)} KB/s`;
  return `${bytesPerSec} B/s`;
}

/** 显示名回退：空名 → 类型标签 + 绑定地址 */
export function tunnelDisplayName(def: TunnelDef): string {
  if (def.name.trim()) return def.name;
  const kindLabel =
    def.kind === 'local'
      ? tNow('state.kindLocal')
      : def.kind === 'remote'
        ? tNow('state.kindRemote')
        : 'SOCKS';
  return `${kindLabel} ${def.bindHost}:${def.bindPort}`;
}

/** §9.6 连接反馈文案：返回 null 表示无需提示（无关联隧道或全部成功且为空） */
export function tunnelFeedback(
  sessionName: string,
  results: { name: string; bind: string; ok: boolean; error?: string | null }[],
): { level: 'info' | 'error'; message: string } | null {
  if (results.length === 0) return null;
  const ok = results.filter((r) => r.ok).length;
  if (ok === results.length) {
    return {
      level: 'info',
      message: tNow('state.tunnelsAllStarted', {
        name: sessionName,
        ok,
        total: results.length,
      }),
    };
  }
  const first = results.find((r) => !r.ok);
  const label = first ? `${first.name || first.bind}` : '';
  const rest = results.length - ok - 1;
  const vars = { name: sessionName, tunnel: label, error: first?.error ?? '', count: rest };
  const key = first?.error
    ? rest > 0
      ? 'state.tunnelStartFailedErrMore'
      : 'state.tunnelStartFailedErr'
    : rest > 0
      ? 'state.tunnelStartFailedMore'
      : 'state.tunnelStartFailed';
  return { level: 'error', message: tNow(key, vars) };
}
