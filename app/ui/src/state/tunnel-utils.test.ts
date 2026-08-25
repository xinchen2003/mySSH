import { describe, expect, it } from 'vitest';
import {
  TUNNEL_TEMPLATES,
  draftFromDef,
  draftToDef,
  fmtRate,
  isWildcardBind,
  startModeFlags,
  startModeOf,
  tunnelDisplayName,
  tunnelFeedback,
  validateTunnelDraft,
  type TunnelDraft,
} from './tunnel-utils';
import type { TunnelDef } from '../term/types';

const def = (over: Partial<TunnelDef> = {}): TunnelDef => ({
  id: 'td-1',
  sessionId: 's1',
  kind: 'local',
  name: 'MySQL',
  bindHost: '127.0.0.1',
  bindPort: 13306,
  targetHost: '10.0.0.8',
  targetPort: 3306,
  autostart: false,
  withSession: true,
  createdAt: '2026-08-25 00:00:00',
  ...over,
});

describe('startMode 映射（§9.2）', () => {
  it('随会话优先于自启（旧数据双真归并）', () => {
    expect(startModeOf({ autostart: false, withSession: false })).toBe('manual');
    expect(startModeOf({ autostart: true, withSession: false })).toBe('autostart');
    expect(startModeOf({ autostart: false, withSession: true })).toBe('withSession');
    expect(startModeOf({ autostart: true, withSession: true })).toBe('withSession');
  });

  it('单选落位互斥', () => {
    expect(startModeFlags('withSession')).toEqual({ autostart: false, withSession: true });
    expect(startModeFlags('autostart')).toEqual({ autostart: true, withSession: false });
    expect(startModeFlags('manual')).toEqual({ autostart: false, withSession: false });
  });

  it('draft 往返保持 id 与标记位', () => {
    const d = def({ autostart: true, withSession: false });
    const round = draftToDef(draftFromDef(d), d.createdAt);
    expect(round.id).toBe(d.id);
    expect(round.autostart).toBe(true);
    expect(round.withSession).toBe(false);
    expect(round.createdAt).toBe(d.createdAt);
  });
});

describe('草稿校验（§9.4）', () => {
  const base: TunnelDraft = draftFromDef(def());

  it('合法草稿通过', () => {
    expect(validateTunnelDraft(base)).toBeNull();
    expect(
      validateTunnelDraft({ ...base, kind: 'dynamic', targetHost: '', targetPort: '' }),
    ).toBeNull();
  });

  it('端口范围', () => {
    expect(validateTunnelDraft({ ...base, bindPort: '0' })).toContain('1-65535');
    expect(validateTunnelDraft({ ...base, bindPort: '65536' })).toContain('1-65535');
    expect(validateTunnelDraft({ ...base, bindPort: 'abc' })).toContain('1-65535');
    expect(validateTunnelDraft({ ...base, targetPort: '0' })).toContain('目标端口');
  });

  it('local/remote 必须有目标', () => {
    expect(validateTunnelDraft({ ...base, targetHost: ' ' })).toContain('目标地址');
  });

  it('绑定地址非空', () => {
    expect(validateTunnelDraft({ ...base, bindHost: '' })).toContain('绑定地址');
  });

  it('dynamic 落库清空目标字段', () => {
    const d = draftToDef({ ...base, kind: 'dynamic', targetHost: 'x', targetPort: '80' }, '');
    expect(d.targetHost).toBeNull();
    expect(d.targetPort).toBeNull();
  });
});

describe('模板（§9.5）', () => {
  it('模板只预填端口与类型，SOCKS5 为 dynamic', () => {
    expect(TUNNEL_TEMPLATES.mysql).toMatchObject({ kind: 'local', targetPort: 3306 });
    expect(TUNNEL_TEMPLATES.socks5.kind).toBe('dynamic');
    expect(TUNNEL_TEMPLATES.socks5.targetPort).toBeUndefined();
    for (const t of Object.values(TUNNEL_TEMPLATES)) {
      expect(t.bindPort).toBeGreaterThan(0);
      expect(t.bindPort).toBeLessThanOrEqual(65535);
    }
  });
});

describe('显示与反馈', () => {
  it('通配绑定识别', () => {
    expect(isWildcardBind('0.0.0.0')).toBe(true);
    expect(isWildcardBind('::')).toBe(true);
    expect(isWildcardBind('127.0.0.1')).toBe(false);
    expect(isWildcardBind('192.168.1.5')).toBe(false);
  });

  it('空名回退为类型+绑定', () => {
    expect(tunnelDisplayName(def({ name: '' }))).toBe('本地 127.0.0.1:13306');
    expect(tunnelDisplayName(def())).toBe('MySQL');
  });

  it('速率格式化', () => {
    expect(fmtRate(512)).toBe('512 B/s');
    expect(fmtRate(2048)).toBe('2.0 KB/s');
    expect(fmtRate(3 * 1024 * 1024)).toBe('3.0 MB/s');
  });

  it('§9.6 反馈：全成功 → info；有失败 → error 含首条原因；空 → null', () => {
    expect(tunnelFeedback('web-01', [])).toBeNull();
    const ok = tunnelFeedback('web-01', [
      { name: 'A', bind: '127.0.0.1:1', ok: true },
      { name: 'B', bind: '127.0.0.1:2', ok: true },
    ]);
    expect(ok).toEqual({ level: 'info', message: 'web-01 已连接，2/2 个关联隧道已启动' });
    const bad = tunnelFeedback('web-01', [
      { name: 'MySQL', bind: '127.0.0.1:3307', ok: false, error: 'E4001 端口被占用' },
      { name: 'B', bind: 'b', ok: false, error: 'x' },
      { name: 'C', bind: 'c', ok: true },
    ]);
    expect(bad?.level).toBe('error');
    expect(bad?.message).toContain('MySQL');
    expect(bad?.message).toContain('E4001 端口被占用');
    expect(bad?.message).toContain('另有 1 条失败');
  });
});
