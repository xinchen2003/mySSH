import { useEffect, useState } from 'react';
import type { MsgKey } from '../i18n';
import { useAppStore } from '../state/app-store';
import { invoke } from '@tauri-apps/api/core';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { BUILTIN_THEMES } from '../term/themes';
import { KEY_ACTIONS, keymapFromSettings, type KeymapScheme } from '../term/keymap';
import { readTermBackground, readTerminalSettings } from '../state/apply-settings';
import { Dialog } from './Dialog';
import { useT } from '../i18n';

const inputCls =
  'rounded border border-neutral-700 bg-neutral-800 px-2 py-1 text-xs text-neutral-200 outline-none focus:border-blue-500 focus-visible:ring-1 focus-visible:ring-neutral-500';

/** 终端字体候选（等宽）；document.fonts.check 探测系统已装，探测失败则全量列出 */
const FONT_CANDIDATES = [
  'Cascadia Code',
  'Cascadia Mono',
  'Consolas',
  'JetBrains Mono',
  'Fira Code',
  'Source Code Pro',
  'Courier New',
  'Lucida Console',
  'MS Gothic',
  'NSimSun',
  '等线',
];

function installedFonts(): string[] {
  try {
    return FONT_CANDIDATES.filter((f) => document.fonts.check(`12px "${f}"`));
  } catch {
    return FONT_CANDIDATES;
  }
}

/** MCP 客户端配置片段：omp/pi 用 .omp/mcp.json；Claude Code 同构（mcpServers.http）；
 *  OpenCode 用 opencode.json 的 mcp 键 + type remote */
function mcpConfigOmp(port: number, token: string): string {
  return JSON.stringify(
    {
      mcpServers: {
        myssh: {
          type: 'http',
          url: `http://127.0.0.1:${port}/mcp`,
          headers: { Authorization: `Bearer ${token}` },
        },
      },
    },
    null,
    2,
  );
}
function mcpConfigClaude(port: number, token: string): string {
  return mcpConfigOmp(port, token);
}
function mcpConfigOpencode(port: number, token: string): string {
  return JSON.stringify(
    {
      mcp: {
        myssh: {
          type: 'remote',
          url: `http://127.0.0.1:${port}/mcp`,
          headers: { Authorization: `Bearer ${token}` },
        },
      },
    },
    null,
    2,
  );
}

/** 审计记录（后端 audit_query 的 DTO 镜像，camelCase） */
interface AuditRow {
  id: number;
  ts: string;
  actor: string;
  sessionId: string | null;
  action: string;
  detail: unknown;
}
interface AuditPage {
  records: AuditRow[];
  nextCursor: number | null;
}

const ACTOR_BADGE: Record<string, { cls: string; labelKey: MsgKey }> = {
  gui: { cls: 'bg-neutral-700 text-neutral-300', labelKey: 'dialogs.auditActorGui' },
  mcp: { cls: 'bg-blue-900/60 text-blue-300', labelKey: 'dialogs.auditActorMcp' },
  cli: { cls: 'bg-purple-900/60 text-purple-300', labelKey: 'dialogs.auditActorCli' },
};

export function SettingsDialog() {
  const settings = useAppStore((s) => s.settings);
  const setSetting = useAppStore((s) => s.setSetting);
  const toggleSettings = useAppStore((s) => s.toggleSettings);
  const t = useT();

  const theme = typeof settings['theme'] === 'string' ? settings['theme'] : 'one-dark';
  const lang = settings['ui.language'] === 'en-US' ? 'en-US' : 'zh-CN';
  const customJson =
    typeof settings['theme.customJson'] === 'string' ? settings['theme.customJson'] : '';
  const term = readTerminalSettings(settings);
  const termBg = readTermBackground(settings);
  const bgImage = termBg.image;
  const bgOpacity = termBg.opacity;
  // MCP 服务端（批次二十一）：端口/令牌读 settings KV；启停走 mcp_restart 热生效
  const mcpPortRaw = settings['mcp.port'];
  const mcpPort = typeof mcpPortRaw === 'number' ? mcpPortRaw : 17345;
  const mcpToken = typeof settings['mcp.token'] === 'string' ? settings['mcp.token'] : '';
  const notify = useAppStore((s) => s.notify);
  /** MCP 设置写入：setSetting 落库是 fire-and-forget，须等 settings_set 完成再 mcp_restart，否则读到旧值 */
  const setMcp = async (key: string, value: unknown) => {
    setSetting(key, value);
    try {
      await invoke('settings_set', { key, value });
      await invoke('mcp_restart');
    } catch {
      // 落库/重启失败：设置面板下次打开按库中值渲染，不阻断 UI
    }
  };
  // 批次十一 8：断线重连次数（0-20，默认 5）
  const reconnectRaw = settings['terminal.reconnectAttempts'];
  const reconnectAttempts =
    typeof reconnectRaw === 'number' && reconnectRaw >= 0 && reconnectRaw <= 20
      ? Math.trunc(reconnectRaw)
      : 5;
  const schemeRaw = settings['keymap.scheme'];
  const scheme: KeymapScheme = schemeRaw === 'vim' || schemeRaw === 'emacs' ? schemeRaw : 'default';
  const bindings = keymapFromSettings(settings);
  // fontFamily 是 CSS 字体栈（"'Cascadia Code', 'JetBrains Mono', Consolas, monospace"）。
  // 取栈中首个命中的候选字体作为当前选中；都不命中（自定义值）时置顶原值
  const fonts = installedFonts();
  const currentFont = fonts.find((f) => term.fontFamily.includes(f)) ?? term.fontFamily;
  const fontOptions = fonts.includes(currentFont) ? fonts : [currentFont, ...fonts];

  // AI 审计（修改清单 C1）：弹窗打开即加载第一页；过滤是客户端对已加载页做的
  const sessions = useAppStore((s) => s.sessions);
  const [auditRows, setAuditRows] = useState<AuditRow[]>([]);
  const [auditNext, setAuditNext] = useState<number | null>(null);
  const [auditLoading, setAuditLoading] = useState(true);
  const [auditActor, setAuditActor] = useState('');
  const [auditAction, setAuditAction] = useState('');
  // 「加载更多」：onClick 先置 loading 再调用；追加到已加载记录尾部
  const loadMoreAudit = async (cursor: number) => {
    try {
      const page = await invoke<AuditPage>('audit_query', { cursor, limit: 50 });
      setAuditRows((prev) => [...prev, ...page.records]);
      setAuditNext(page.nextCursor);
    } catch {
      // 查询失败不阻断设置面板；保留已加载数据
    } finally {
      setAuditLoading(false);
    }
  };
  // 弹窗打开（挂载）即拉第一页；setState 只在 Promise 回调里（react-hooks/set-state-in-effect）
  useEffect(() => {
    invoke<AuditPage>('audit_query', { cursor: null, limit: 50 })
      .then((page) => {
        setAuditRows(page.records);
        setAuditNext(page.nextCursor);
      })
      .catch(() => undefined) // 查询失败不阻断设置面板
      .finally(() => setAuditLoading(false));
  }, []);
  const auditFiltered = auditRows.filter(
    (r) =>
      (auditActor === '' || r.actor === auditActor) &&
      (auditAction === '' || r.action.toLowerCase().includes(auditAction.toLowerCase())),
  );
  const sessionName = (id: string | null) => {
    if (id === null) return '-';
    return sessions.find((s) => s.id === id)?.name ?? id.slice(0, 8);
  };
  /** 导出已加载记录为 JSON 文件（文件名带本地时间戳） */
  const exportAudit = () => {
    const pad = (n: number) => String(n).padStart(2, '0');
    const d = new Date();
    const stamp = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}-${pad(d.getHours())}${pad(d.getMinutes())}${pad(d.getSeconds())}`;
    const blob = new Blob([JSON.stringify(auditRows, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `myssh-audit-${stamp}.json`;
    a.click();
    URL.revokeObjectURL(url);
  };

  return (
    <Dialog
      title={t('dialogs.settingsTitle')}
      onClose={toggleSettings}
      panelClass="max-h-[80vh] overscroll-contain w-[560px] overflow-y-auto rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-xs text-neutral-300 shadow-xl"
    >
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-neutral-100">{t('dialogs.settingsTitle')}</h2>
        <button
          className="rounded px-1 text-neutral-500 hover:text-neutral-200"
          onClick={toggleSettings}
          aria-label={t('dialogs.closeSettings')}
        >
          ✕
        </button>
      </div>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">语言 / Language</h3>
        <select
          className={inputCls}
          aria-label={t('dialogs.languageAria')}
          value={lang}
          onChange={(e) => setSetting('ui.language', e.target.value)}
        >
          <option value="zh-CN">简体中文</option>
          <option value="en-US">English</option>
        </select>
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.theme')}</h3>
        <div className="flex items-center gap-2">
          <select
            className={inputCls}
            aria-label={t('dialogs.theme')}
            value={theme}
            onChange={(e) => setSetting('theme', e.target.value)}
          >
            <option value="system">{t('dialogs.themeSystem')}</option>
            {BUILTIN_THEMES.map((t) => (
              <option key={t.id} value={t.id}>
                {t.label}
              </option>
            ))}
            <option value="custom">{t('dialogs.themeCustom')}</option>
          </select>
          <span className="text-neutral-500">{t('dialogs.themeChromeNote')}</span>
        </div>
        {theme === 'custom' && (
          <textarea
            className={`${inputCls} mt-2 h-28 w-full font-mono`}
            aria-label={t('dialogs.customThemeJsonAria')}
            spellCheck={false}
            placeholder='{"ui":"dark","background":"#1e1e1e","foreground":"#d4d4d4",…}'
            value={customJson}
            onChange={(e) => setSetting('theme.customJson', e.target.value)}
          />
        )}
        <div className="mt-2 flex items-center gap-2">
          <span className="text-neutral-500">{t('dialogs.backgroundImage')}</span>
          <label className="cursor-pointer rounded bg-neutral-800 px-2 py-0.5 text-neutral-300 hover:bg-neutral-700">
            {t('dialogs.backgroundImageChoose')}
            <input
              type="file"
              accept="image/*"
              className="hidden"
              aria-label={t('dialogs.backgroundImageChooseAria')}
              onChange={(e) => {
                const f = e.target.files?.[0];
                e.target.value = '';
                if (!f) return;
                // data URL 存 settings KV（本地优先，免走 asset 协议）
                const r = new FileReader();
                r.onload = () =>
                  setSetting(
                    'terminal.backgroundImage',
                    typeof r.result === 'string' ? r.result : '',
                  );
                r.readAsDataURL(f);
              }}
            />
          </label>
          {bgImage && (
            <button
              className="rounded px-2 py-0.5 text-neutral-400 hover:bg-neutral-800"
              onClick={() => setSetting('terminal.backgroundImage', '')}
            >
              {t('dialogs.backgroundImageClear')}
            </button>
          )}
        </div>
        {bgImage && (
          <div className="mt-2 flex items-center gap-2">
            <label htmlFor="set-bg-opacity" className="text-neutral-500">
              {t('dialogs.backgroundOpacity')}
            </label>
            <input
              id="set-bg-opacity"
              type="range"
              min={5}
              max={100}
              step={5}
              className="w-40"
              value={Math.round(bgOpacity * 100)}
              onChange={(e) =>
                setSetting('terminal.backgroundOpacity', Number(e.target.value) / 100)
              }
            />
            <span className="tabular-nums text-neutral-500">{Math.round(bgOpacity * 100)}%</span>
          </div>
        )}
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.terminal')}</h3>
        <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
          <label htmlFor="set-font">{t('dialogs.font')}</label>
          <select
            id="set-font"
            className={inputCls}
            value={currentFont}
            onChange={(e) => setSetting('terminal.fontFamily', `'${e.target.value}', monospace`)}
          >
            {fontOptions.map((f) => (
              <option key={f} value={f}>
                {f}
              </option>
            ))}
          </select>
          <label htmlFor="set-size">{t('dialogs.fontSize')}</label>
          <input
            id="set-size"
            className={`${inputCls} w-20`}
            type="number"
            min={8}
            max={32}
            value={term.fontSize}
            onChange={(e) => setSetting('terminal.fontSize', Number(e.target.value))}
          />
          <label htmlFor="set-reconnect">{t('dialogs.reconnectAttempts')}</label>
          <span className="flex items-center gap-2">
            <input
              id="set-reconnect"
              className={`${inputCls} w-20`}
              type="number"
              min={0}
              max={20}
              value={reconnectAttempts}
              onChange={(e) =>
                setSetting(
                  'terminal.reconnectAttempts',
                  Math.min(20, Math.max(0, Math.trunc(Number(e.target.value) || 0))),
                )
              }
            />
            <span className="text-neutral-500">{t('dialogs.reconnectHint')}</span>
          </span>
          <label htmlFor="set-copy-sel">{t('dialogs.copyOnSelect')}</label>
          <span className="flex items-center gap-2">
            <input
              id="set-copy-sel"
              type="checkbox"
              checked={settings['terminal.copyOnSelect'] !== false}
              onChange={(e) => setSetting('terminal.copyOnSelect', e.target.checked)}
            />
            <span className="text-neutral-500">{t('dialogs.copyOnSelectHint')}</span>
          </span>
          <label htmlFor="set-rc-paste">{t('dialogs.rightClickPaste')}</label>
          <span className="flex items-center gap-2">
            <input
              id="set-rc-paste"
              type="checkbox"
              checked={settings['terminal.rightClickPaste'] === true}
              onChange={(e) => setSetting('terminal.rightClickPaste', e.target.checked)}
            />
            <span className="text-neutral-500">{t('dialogs.rightClickPasteHint')}</span>
          </span>
          <label htmlFor="set-confirm-close">{t('dialogs.confirmClose')}</label>
          <span className="flex items-center gap-2">
            <input
              id="set-confirm-close"
              type="checkbox"
              checked={settings['terminal.confirmCloseTab'] !== false}
              onChange={(e) => setSetting('terminal.confirmCloseTab', e.target.checked)}
            />
            <span className="text-neutral-500">{t('dialogs.confirmCloseHint')}</span>
          </span>
          <label htmlFor="set-bell">{t('dialogs.bell')}</label>
          <span className="flex items-center gap-2">
            <input
              id="set-bell"
              type="checkbox"
              checked={settings['terminal.bell'] !== false}
              onChange={(e) => setSetting('terminal.bell', e.target.checked)}
            />
            <span className="text-neutral-500">{t('dialogs.bellHint')}</span>
          </span>
        </div>
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.uiSection')}</h3>
        <label className="flex items-center gap-2" htmlFor="set-statusbar">
          <input
            id="set-statusbar"
            type="checkbox"
            checked={settings['ui.statusBar'] !== false}
            onChange={(e) => setSetting('ui.statusBar', e.target.checked)}
          />
          <span className="text-neutral-500">{t('dialogs.statusBarHint')}</span>
        </label>
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.sidebarSection')}</h3>
        <label className="flex items-center gap-2" htmlFor="set-click-connect">
          <input
            id="set-click-connect"
            type="checkbox"
            checked={settings['sidebar.clickToConnect'] === true}
            onChange={(e) => setSetting('sidebar.clickToConnect', e.target.checked)}
          />
          <span className="text-neutral-500">{t('dialogs.clickToConnectHint')}</span>
        </label>
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">MCP</h3>
        <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
          <label htmlFor="set-mcp-enabled">{t('dialogs.mcpEnabled')}</label>
          <input
            id="set-mcp-enabled"
            type="checkbox"
            checked={settings['mcp.enabled'] === true}
            onChange={(e) => {
              void setMcp('mcp.enabled', e.target.checked);
            }}
          />
          <label htmlFor="set-mcp-port">{t('dialogs.mcpPort')}</label>
          <input
            id="set-mcp-port"
            type="number"
            className={inputCls}
            min={1024}
            max={65535}
            value={mcpPort}
            onChange={(e) => {
              const v = Math.trunc(Number(e.target.value));
              if (v >= 1024 && v <= 65535) void setMcp('mcp.port', v);
            }}
          />
          <label htmlFor="set-mcp-token">{t('dialogs.mcpToken')}</label>
          <span className="flex items-center gap-2">
            <input
              id="set-mcp-token"
              className={`${inputCls} min-w-0 flex-1 font-mono`}
              spellCheck={false}
              value={mcpToken}
              placeholder={t('dialogs.mcpTokenPlaceholder')}
              onChange={(e) => void setMcp('mcp.token', e.target.value)}
            />
            <button
              className="shrink-0 rounded px-2 py-0.5 text-neutral-400 hover:bg-neutral-800"
              onClick={() => {
                const tok = Array.from(crypto.getRandomValues(new Uint8Array(16)))
                  .map((b) => b.toString(16).padStart(2, '0'))
                  .join('');
                void setMcp('mcp.token', tok);
              }}
            >
              {t('dialogs.mcpTokenRegen')}
            </button>
          </span>
          <span>{t('dialogs.mcpToolPerms')}</span>
          <span className="flex flex-wrap items-center gap-x-4 gap-y-1">
            {/* 分组开关落 mcp.allow.*，缺省视为允许；setMcp 写完触发 mcp_restart 热生效 */}
            {(
              [
                ['mcp.allow.list_sessions', 'dialogs.mcpAllowListSessions'],
                ['mcp.allow.ssh_exec', 'dialogs.mcpAllowSshExec'],
                ['mcp.allow.sftp_read', 'dialogs.mcpAllowSftpRead'],
                ['mcp.allow.sftp_write', 'dialogs.mcpAllowSftpWrite'],
                ['mcp.allow.sftp_transfer', 'dialogs.mcpAllowSftpTransfer'],
              ] as const
            ).map(([key, labelKey]) => (
              <label key={key} className="flex items-center gap-1">
                <input
                  type="checkbox"
                  checked={settings[key] !== false}
                  onChange={(e) => void setMcp(key, e.target.checked)}
                />
                {t(labelKey)}
              </label>
            ))}
          </span>
          {mcpToken && (
            <>
              <span>{t('dialogs.mcpCopyConfig')}</span>
              <span className="flex flex-wrap items-center gap-1.5">
                {(
                  [
                    ['omp', mcpConfigOmp(mcpPort, mcpToken)],
                    ['Claude Code', mcpConfigClaude(mcpPort, mcpToken)],
                    ['OpenCode', mcpConfigOpencode(mcpPort, mcpToken)],
                  ] as const
                ).map(([label, json]) => (
                  <button
                    key={label}
                    className="rounded bg-neutral-800 px-2 py-0.5 text-neutral-300 hover:bg-neutral-700"
                    onClick={() => {
                      void writeText(json)
                        .then(() =>
                          notify(t('dialogs.mcpConfigCopied', { agent: label }), 'success'),
                        )
                        .catch(() => undefined);
                    }}
                  >
                    {label}
                  </button>
                ))}
              </span>
            </>
          )}
        </div>
        <p className="mt-1.5 text-neutral-600">{t('dialogs.mcpHint')}</p>
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.auditSection')}</h3>
        <div className="mb-2 flex items-center gap-2">
          <select
            className={inputCls}
            aria-label={t('dialogs.auditColActor')}
            value={auditActor}
            onChange={(e) => setAuditActor(e.target.value)}
          >
            <option value="">{t('dialogs.auditActorAll')}</option>
            <option value="gui">{t('dialogs.auditActorGui')}</option>
            <option value="mcp">{t('dialogs.auditActorMcp')}</option>
            <option value="cli">{t('dialogs.auditActorCli')}</option>
          </select>
          <input
            className={`${inputCls} min-w-0 flex-1`}
            aria-label={t('dialogs.auditActionPlaceholder')}
            placeholder={t('dialogs.auditActionPlaceholder')}
            value={auditAction}
            onChange={(e) => setAuditAction(e.target.value)}
          />
          <button
            className="shrink-0 rounded bg-neutral-800 px-2 py-1 text-neutral-300 hover:bg-neutral-700 disabled:opacity-40"
            disabled={auditRows.length === 0}
            onClick={exportAudit}
          >
            {t('dialogs.auditExport')}
          </button>
        </div>
        <table className="w-full table-fixed text-left">
          <thead>
            <tr className="text-neutral-500">
              <th className="w-32 py-0.5 pr-2 font-normal">{t('dialogs.auditColTime')}</th>
              <th className="w-14 py-0.5 pr-2 font-normal">{t('dialogs.auditColActor')}</th>
              <th className="w-24 py-0.5 pr-2 font-normal">{t('dialogs.auditColSession')}</th>
              <th className="w-28 py-0.5 pr-2 font-normal">{t('dialogs.auditColAction')}</th>
              <th className="py-0.5 font-normal">{t('dialogs.auditColDetail')}</th>
            </tr>
          </thead>
          <tbody>
            {auditFiltered.map((r) => {
              const detail = JSON.stringify(r.detail);
              const badge = ACTOR_BADGE[r.actor];
              return (
                <tr key={r.id} className="border-t border-neutral-800/50">
                  <td className="truncate py-0.5 pr-2 text-neutral-400">{r.ts}</td>
                  <td className="py-0.5 pr-2">
                    <span
                      className={`rounded px-1.5 py-0.5 ${badge?.cls ?? 'bg-neutral-700 text-neutral-300'}`}
                    >
                      {badge ? t(badge.labelKey) : r.actor}
                    </span>
                  </td>
                  <td className="truncate py-0.5 pr-2 text-neutral-400">
                    {sessionName(r.sessionId)}
                  </td>
                  <td className="truncate py-0.5 pr-2">{r.action}</td>
                  <td className="truncate py-0.5 font-mono text-neutral-500" title={detail}>
                    {detail}
                  </td>
                </tr>
              );
            })}
            {auditFiltered.length === 0 && (
              <tr className="border-t border-neutral-800/50">
                <td colSpan={5} className="py-2 text-center text-neutral-600">
                  {auditLoading ? t('dialogs.auditLoading') : t('dialogs.auditEmpty')}
                </td>
              </tr>
            )}
          </tbody>
        </table>
        {auditNext !== null && (
          <button
            className="mt-2 rounded bg-neutral-800 px-2 py-1 text-neutral-300 hover:bg-neutral-700 disabled:opacity-40"
            disabled={auditLoading}
            onClick={() => {
              setAuditLoading(true);
              void loadMoreAudit(auditNext);
            }}
          >
            {auditLoading ? t('dialogs.auditLoading') : t('dialogs.auditLoadMore')}
          </button>
        )}
      </section>

      <section>
        <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.shortcuts')}</h3>
        <div className="mb-2 flex items-center gap-2">
          <label htmlFor="set-scheme">{t('dialogs.keymapScheme')}</label>
          <select
            id="set-scheme"
            className={inputCls}
            value={scheme}
            onChange={(e) => setSetting('keymap.scheme', e.target.value)}
          >
            <option value="default">{t('dialogs.schemeDefault')}</option>
            <option value="vim">{t('dialogs.schemeVim')}</option>
            <option value="emacs">Emacs</option>
          </select>
        </div>
        <table className="mb-2 w-full text-left">
          <tbody>
            {KEY_ACTIONS.map((a) => (
              <tr key={a.id} className="border-t border-neutral-800/50">
                <td className="py-0.5 pr-2">{a.label}</td>
                <td className="py-0.5 text-right font-mono text-neutral-400">
                  {bindings[a.id]}
                  {a.alias ? ` / ${a.alias}` : ''}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </Dialog>
  );
}
