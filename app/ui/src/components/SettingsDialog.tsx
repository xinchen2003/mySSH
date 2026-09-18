import { useState } from 'react';
import { useAppStore } from '../state/app-store';
import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { BUILTIN_THEMES, SYSTEM_DEFAULTS } from '../term/themes';
import { KEY_ACTIONS, keymapFromSettings, type KeymapScheme } from '../term/keymap';
import { readTermBackground, readTerminalSettings } from '../state/apply-settings';
import { Dialog } from './Dialog';
import { ThemeStudio } from './ThemeStudio';
import { useT, type MsgKey } from '../i18n';

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

/** 左导航页签 */
type NavTab = 'general' | 'terminal' | 'appearance' | 'mcp' | 'shortcuts';
const NAV_TABS: [NavTab, MsgKey][] = [
  ['general', 'dialogs.navGeneral'],
  ['terminal', 'dialogs.navTerminal'],
  ['appearance', 'dialogs.navAppearance'],
  ['mcp', 'dialogs.navMcp'],
  ['shortcuts', 'dialogs.navShortcuts'],
];

/** 单张主题卡：迷你终端预览 + 名称 + 明暗徽章；点击即切换 */
function ThemeCard({
  id,
  label,
  ui,
  preview,
  selected,
  onPick,
}: {
  id: string;
  label: string;
  ui?: 'dark' | 'light';
  preview: React.ReactNode;
  selected: boolean;
  onPick: (id: string) => void;
}) {
  const t = useT();
  return (
    <button
      className={`rounded border border-neutral-700 p-1.5 text-left transition hover:bg-neutral-800/60 ${
        selected ? 'ring-2 ring-blue-500' : ''
      }`}
      aria-pressed={selected}
      onClick={() => onPick(id)}
    >
      <div className="mb-1 h-14 overflow-hidden rounded">{preview}</div>
      <div className="flex items-center justify-between px-0.5">
        <span className="text-neutral-200">{label}</span>
        {ui && (
          <span className="rounded bg-neutral-800 px-1 text-[10px] text-neutral-400">
            {ui === 'dark' ? t('dialogs.themeDarkBadge') : t('dialogs.themeLightBadge')}
          </span>
        )}
      </div>
    </button>
  );
}

/** 内置主题卡预览：主题底色 + 前景色一行伪文本 + 4 个 ANSI 彩色小块 */
function BuiltinPreview({ themeId }: { themeId: string }) {
  const def = BUILTIN_THEMES.find((b) => b.id === themeId);
  const x = def?.xterm ?? {};
  return (
    <div className="flex h-full flex-col gap-1 p-1.5" style={{ background: x.background }}>
      <div className="h-1 w-3/4 rounded-sm" style={{ background: x.foreground }} />
      <div className="h-1 w-1/2 rounded-sm" style={{ background: x.foreground, opacity: 0.6 }} />
      <div className="flex gap-1">
        {[x.red, x.green, x.yellow, x.blue].map((c, i) => (
          <div key={i} className="h-2.5 w-2.5 rounded-sm" style={{ background: c }} />
        ))}
      </div>
    </div>
  );
}

export function SettingsDialog() {
  const settings = useAppStore((s) => s.settings);
  const setSetting = useAppStore((s) => s.setSetting);
  const toggleSettings = useAppStore((s) => s.toggleSettings);
  const t = useT();
  const [tab, setTab] = useState<NavTab>('general');

  const theme = typeof settings['theme'] === 'string' ? settings['theme'] : 'one-dark';
  const lang = settings['ui.language'] === 'en-US' ? 'en-US' : 'zh-CN';
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

  // 跟随系统卡：半深半浅对半分预览（取系统默认深浅主题底色）
  const sysDark = BUILTIN_THEMES.find((b) => b.id === SYSTEM_DEFAULTS.dark)?.xterm.background;
  const sysLight = BUILTIN_THEMES.find((b) => b.id === SYSTEM_DEFAULTS.light)?.xterm.background;

  return (
    <Dialog
      title={t('dialogs.settingsTitle')}
      onClose={toggleSettings}
      panelClass="flex max-h-[80vh] w-[720px] flex-col overscroll-contain rounded-lg border border-neutral-700 bg-neutral-900 text-xs text-neutral-300 shadow-xl"
    >
      <div className="flex items-center justify-between border-b border-neutral-800 p-4 pb-3">
        <h2 className="text-sm font-semibold text-neutral-100">{t('dialogs.settingsTitle')}</h2>
        <button
          className="rounded px-1 text-neutral-500 hover:text-neutral-200"
          onClick={toggleSettings}
          aria-label={t('dialogs.closeSettings')}
        >
          ✕
        </button>
      </div>

      <div className="flex min-h-0 flex-1">
        {/* 左导航 */}
        <nav className="w-36 shrink-0 border-r border-neutral-800 p-2">
          {NAV_TABS.map(([id, labelKey]) => (
            <button
              key={id}
              className={`mb-0.5 block w-full rounded-r border-l-2 px-2 py-1.5 text-left ${
                tab === id
                  ? 'border-blue-500 bg-neutral-800 text-neutral-100'
                  : 'border-transparent text-neutral-400 hover:bg-neutral-800/50'
              }`}
              onClick={() => setTab(id)}
            >
              {t(labelKey)}
            </button>
          ))}
        </nav>

        {/* 右内容区 */}
        <div className="min-w-0 flex-1 overflow-y-auto p-4">
          {tab === 'general' && (
            <>
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

              <section>
                <h3 className="mb-1.5 font-semibold text-neutral-200">
                  {t('dialogs.sidebarSection')}
                </h3>
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

              {/* 轴一 1.2：一键导出诊断包（日志可能含主机地址/用户名，文案提示用户自审） */}
              <section>
                <h3 className="mb-1.5 font-semibold text-neutral-200">
                  {t('dialogs.diagSection')}
                </h3>
                <button
                  className="rounded bg-neutral-800 px-2 py-1 text-xs text-neutral-300 hover:bg-neutral-700"
                  onClick={() => {
                    void (async () => {
                      const path = await save({
                        defaultPath: 'myssh-diagnostics.zip',
                        filters: [{ name: 'ZIP', extensions: ['zip'] }],
                      });
                      if (!path) return;
                      try {
                        await invoke('export_diagnostics', { path });
                        notify(t('dialogs.diagExported', { path }), 'success');
                      } catch (e) {
                        notify(t('dialogs.diagFailed', { msg: String(e) }), 'error');
                      }
                    })();
                  }}
                >
                  {t('dialogs.diagExport')}
                </button>
                <p className="mt-1 text-xs text-neutral-500">{t('dialogs.diagHint')}</p>
              </section>
            </>
          )}

          {tab === 'terminal' && (
            <section>
              <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.terminal')}</h3>
              <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
                <label htmlFor="set-font">{t('dialogs.font')}</label>
                <select
                  id="set-font"
                  className={inputCls}
                  value={currentFont}
                  onChange={(e) =>
                    setSetting('terminal.fontFamily', `'${e.target.value}', monospace`)
                  }
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
          )}

          {tab === 'appearance' && (
            <>
              <section className="mb-4">
                <h3 className="mb-1.5 font-semibold text-neutral-200">{t('dialogs.theme')}</h3>
                <div className="grid grid-cols-3 gap-2">
                  <ThemeCard
                    id="system"
                    label={t('dialogs.themeCardSystem')}
                    selected={theme === 'system'}
                    onPick={(id) => setSetting('theme', id)}
                    preview={
                      <div
                        className="h-full"
                        style={{
                          background: `linear-gradient(90deg, ${sysDark ?? '#282c34'} 50%, ${sysLight ?? '#fdf6e3'} 50%)`,
                        }}
                      />
                    }
                  />
                  {BUILTIN_THEMES.map((def) => (
                    <ThemeCard
                      key={def.id}
                      id={def.id}
                      label={def.label}
                      ui={def.ui}
                      selected={theme === def.id}
                      onPick={(id) => setSetting('theme', id)}
                      preview={<BuiltinPreview themeId={def.id} />}
                    />
                  ))}
                  <ThemeCard
                    id="custom"
                    label={t('dialogs.themeCardCustom')}
                    selected={theme === 'custom'}
                    onPick={(id) => setSetting('theme', id)}
                    preview={
                      <div className="flex h-full items-center justify-center gap-1 bg-neutral-800 text-lg text-neutral-400">
                        <span>◐</span>
                        {['#e05561', '#8cc265', '#4aa5f0'].map((c) => (
                          <span
                            key={c}
                            className="inline-block h-2.5 w-2.5 rounded-full"
                            style={{ background: c }}
                          />
                        ))}
                      </div>
                    }
                  />
                </div>
                <p className="mt-1.5 text-neutral-500">{t('dialogs.themeChromeNote')}</p>
                {theme === 'custom' && <ThemeStudio />}
              </section>

              <section>
                <div className="flex items-center gap-2">
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
                    <span className="tabular-nums text-neutral-500">
                      {Math.round(bgOpacity * 100)}%
                    </span>
                  </div>
                )}
              </section>
            </>
          )}

          {tab === 'mcp' && (
            <section>
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
          )}

          {tab === 'shortcuts' && (
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
          )}
        </div>
      </div>
    </Dialog>
  );
}
