import { useAppStore } from '../state/app-store';
import { BUILTIN_THEMES } from '../term/themes';
import { KEY_ACTIONS, keymapFromSettings, type KeymapScheme } from '../term/keymap';
import { readTerminalSettings } from '../state/apply-settings';
import { Dialog } from './Dialog';

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

  const theme = typeof settings['theme'] === 'string' ? settings['theme'] : 'one-dark';
  const customJson =
    typeof settings['theme.customJson'] === 'string' ? settings['theme.customJson'] : '';
  const term = readTerminalSettings(settings);
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
      title="设置"
      onClose={toggleSettings}
      panelClass="max-h-[80vh] overscroll-contain w-[560px] overflow-y-auto rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-xs text-neutral-300 shadow-xl"
    >
      <div className="mb-3 flex items-center justify-between">
        <h2 className="text-sm font-semibold text-neutral-100">设置</h2>
        <button
          className="rounded px-1 text-neutral-500 hover:text-neutral-200"
          onClick={toggleSettings}
          aria-label="关闭设置"
        >
          ✕
        </button>
      </div>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">主题</h3>
        <div className="flex items-center gap-2">
          <select
            className={inputCls}
            aria-label="主题"
            value={theme}
            onChange={(e) => setSetting('theme', e.target.value)}
          >
            <option value="system">跟随系统</option>
            {BUILTIN_THEMES.map((t) => (
              <option key={t.id} value={t.id}>
                {t.label}
              </option>
            ))}
            <option value="custom">自定义 JSON</option>
          </select>
          <span className="text-neutral-500">chrome 明暗档随主题 UI 字段切换</span>
        </div>
        {theme === 'custom' && (
          <textarea
            className={`${inputCls} mt-2 h-28 w-full font-mono`}
            aria-label="自定义主题 JSON"
            spellCheck={false}
            placeholder='{"ui":"dark","background":"#1e1e1e","foreground":"#d4d4d4",…}'
            value={customJson}
            onChange={(e) => setSetting('theme.customJson', e.target.value)}
          />
        )}
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">终端</h3>
        <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
          <label htmlFor="set-font">字体</label>
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
          <label htmlFor="set-size">字号</label>
          <input
            id="set-size"
            className={`${inputCls} w-20`}
            type="number"
            min={8}
            max={32}
            value={term.fontSize}
            onChange={(e) => setSetting('terminal.fontSize', Number(e.target.value))}
          />
          <label htmlFor="set-reconnect">断线重连次数</label>
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
            <span className="text-neutral-500">
              0-20，默认 5；已建立的会话重连耗尽提示按挂载时读取
            </span>
          </span>
          <label htmlFor="set-copy-sel">选中即复制</label>
          <span className="flex items-center gap-2">
            <input
              id="set-copy-sel"
              type="checkbox"
              checked={settings['terminal.copyOnSelect'] !== false}
              onChange={(e) => setSetting('terminal.copyOnSelect', e.target.checked)}
            />
            <span className="text-neutral-500">选中终端文本后自动复制（对已有终端立即生效）</span>
          </span>
          <label htmlFor="set-rc-paste">右键直接粘贴</label>
          <span className="flex items-center gap-2">
            <input
              id="set-rc-paste"
              type="checkbox"
              checked={settings['terminal.rightClickPaste'] === true}
              onChange={(e) => setSetting('terminal.rightClickPaste', e.target.checked)}
            />
            <span className="text-neutral-500">终端内右键直接粘贴，而不是打开菜单（默认关闭）</span>
          </span>
          <label htmlFor="set-confirm-close">关闭确认</label>
          <span className="flex items-center gap-2">
            <input
              id="set-confirm-close"
              type="checkbox"
              checked={settings['terminal.confirmCloseTab'] !== false}
              onChange={(e) => setSetting('terminal.confirmCloseTab', e.target.checked)}
            />
            <span className="text-neutral-500">关闭有活动连接的标签前确认（默认开启）</span>
          </span>
          <label htmlFor="set-bell">终端响铃提示</label>
          <span className="flex items-center gap-2">
            <input
              id="set-bell"
              type="checkbox"
              checked={settings['terminal.bell'] !== false}
              onChange={(e) => setSetting('terminal.bell', e.target.checked)}
            />
            <span className="text-neutral-500">
              终端 BEL 时标记标签；窗口非活动时闪烁任务栏（默认开启）
            </span>
          </span>
        </div>
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">界面</h3>
        <label className="flex items-center gap-2" htmlFor="set-statusbar">
          <input
            id="set-statusbar"
            type="checkbox"
            checked={settings['ui.statusBar'] !== false}
            onChange={(e) => setSetting('ui.statusBar', e.target.checked)}
          />
          <span className="text-neutral-500">显示状态栏（连接数/隧道数/当前服务器，默认开启）</span>
        </label>
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">会话侧栏</h3>
        <label className="flex items-center gap-2" htmlFor="set-click-connect">
          <input
            id="set-click-connect"
            type="checkbox"
            checked={settings['sidebar.clickToConnect'] === true}
            onChange={(e) => setSetting('sidebar.clickToConnect', e.target.checked)}
          />
          <span className="text-neutral-500">
            单击服务器时立即连接（默认关闭：单击选中，双击或 Enter 连接）
          </span>
        </label>
      </section>

      <section>
        <h3 className="mb-1.5 font-semibold text-neutral-200">快捷键</h3>
        <div className="mb-2 flex items-center gap-2">
          <label htmlFor="set-scheme">键位方案</label>
          <select
            id="set-scheme"
            className={inputCls}
            value={scheme}
            onChange={(e) => setSetting('keymap.scheme', e.target.value)}
          >
            <option value="default">默认</option>
            <option value="vim">Vim（Alt 系）</option>
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
