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
              setSetting('mcp.enabled', e.target.checked);
              void invoke('mcp_restart').catch(() => undefined);
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
              if (v >= 1024 && v <= 65535) {
                setSetting('mcp.port', v);
                void invoke('mcp_restart').catch(() => undefined);
              }
            }}
          />
          <span>{t('dialogs.mcpToken')}</span>
          <span className="flex items-center gap-2">
            <code className="min-w-0 flex-1 truncate rounded bg-neutral-800 px-2 py-0.5 font-mono">
              {mcpToken || '—'}
            </code>
            {mcpToken && (
              <button
                className="shrink-0 rounded px-2 py-0.5 text-neutral-400 hover:bg-neutral-800"
                onClick={() => {
                  void writeText(mcpToken)
                    .then(() => notify(t('dialogs.mcpTokenCopied'), 'success'))
                    .catch(() => undefined);
                }}
              >
                {t('dialogs.mcpTokenCopy')}
              </button>
            )}
          </span>
        </div>
        <p className="mt-1.5 text-neutral-600">{t('dialogs.mcpHint')}</p>
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
