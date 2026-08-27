import { useState } from 'react';
import { check } from '@tauri-apps/plugin-updater';
import { useAppStore } from '../state/app-store';
import { BUILTIN_THEMES } from '../term/themes';
import { KEY_ACTIONS, keymapFromSettings, type KeymapScheme } from '../term/keymap';
import {
  DEFAULT_MENU_FONT,
  DEFAULT_MENU_ICON,
  readTerminalSettings,
} from '../state/apply-settings';
import { Dialog } from './Dialog';

const inputCls =
  'rounded border border-neutral-700 bg-neutral-800 px-2 py-1 text-xs text-neutral-200 outline-none focus:border-blue-500';

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
  const menuFontRaw = settings['ui.menuFontSize'];
  const menuFont =
    typeof menuFontRaw === 'number' && [11, 12, 13, 14].includes(menuFontRaw)
      ? menuFontRaw
      : DEFAULT_MENU_FONT;
  const menuIconRaw = settings['ui.menuIconSize'];
  const menuIcon =
    typeof menuIconRaw === 'number' && [12, 14, 16, 18].includes(menuIconRaw)
      ? menuIconRaw
      : DEFAULT_MENU_ICON;
  const schemeRaw = settings['keymap.scheme'];
  const scheme: KeymapScheme = schemeRaw === 'vim' || schemeRaw === 'emacs' ? schemeRaw : 'default';
  const bindings = keymapFromSettings(settings);
  const [customKeys, setCustomKeys] = useState(
    typeof settings['keymap.custom'] === 'object' && settings['keymap.custom'] !== null
      ? JSON.stringify(settings['keymap.custom'], null, 2)
      : '',
  );

  return (
    <Dialog
      title="设置"
      onClose={toggleSettings}
      panelClass="max-h-[80vh] w-[560px] overflow-y-auto rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-xs text-neutral-300 shadow-xl"
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
            placeholder='{"ui":"dark","background":"#1e1e1e","foreground":"#d4d4d4",…}'
            value={customJson}
            onChange={(e) => setSetting('theme.customJson', e.target.value)}
          />
        )}
      </section>

      <section className="mb-4">
        <h3 className="mb-1.5 font-semibold text-neutral-200">终端</h3>
        <div className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
          <label htmlFor="set-font">字体族</label>
          <input
            id="set-font"
            className={inputCls}
            value={term.fontFamily}
            onChange={(e) => setSetting('terminal.fontFamily', e.target.value)}
          />
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
          <label htmlFor="set-scroll">回滚行数</label>
          <span className="flex items-center gap-2">
            <input
              id="set-scroll"
              className={`${inputCls} w-24`}
              type="number"
              min={1000}
              max={100000}
              step={1000}
              value={term.scrollback}
              onChange={(e) => setSetting('terminal.scrollback', Number(e.target.value))}
            />
            <span className="text-neutral-500">新建终端生效（xterm 不支持在线改）</span>
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
        <div className="mt-2 grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-2">
          <label htmlFor="set-menu-font">菜单字体大小</label>
          <span className="flex items-center gap-2">
            <select
              id="set-menu-font"
              className={`${inputCls} w-20`}
              value={menuFont}
              onChange={(e) => setSetting('ui.menuFontSize', Number(e.target.value))}
            >
              {[11, 12, 13, 14].map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
            <span className="text-neutral-500">右键菜单文字大小（px，默认 12）</span>
          </span>
          <label htmlFor="set-menu-icon">菜单图标大小</label>
          <span className="flex items-center gap-2">
            <select
              id="set-menu-icon"
              className={`${inputCls} w-20`}
              value={menuIcon}
              onChange={(e) => setSetting('ui.menuIconSize', Number(e.target.value))}
            >
              {[12, 14, 16, 18].map((n) => (
                <option key={n} value={n}>
                  {n}
                </option>
              ))}
            </select>
            <span className="text-neutral-500">右键菜单图标尺寸（px，默认 14）</span>
          </span>
        </div>
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
        <details>
          <summary className="cursor-pointer text-neutral-500 hover:text-neutral-300">
            自定义覆盖（JSON：动作 id → 组合键，须含修饰键）
          </summary>
          <textarea
            className={`${inputCls} mt-1 h-20 w-full font-mono`}
            placeholder='{"nextTab":"Alt+L"}'
            value={customKeys}
            onChange={(e) => setCustomKeys(e.target.value)}
            onBlur={() => {
              const t = customKeys.trim();
              if (!t) {
                setSetting('keymap.custom', {});
                return;
              }
              try {
                setSetting('keymap.custom', JSON.parse(t));
              } catch {
                /* 坏 JSON 不落库，留编辑态 */
              }
            }}
          />
        </details>
      </section>

      <section className="mt-4 border-t border-neutral-800 pt-3">
        <h3 className="mb-1.5 font-semibold text-neutral-200">关于</h3>
        <AboutRow />
      </section>
    </Dialog>
  );
}

function AboutRow() {
  const notify = useAppStore((s) => s.notify);
  const [checking, setChecking] = useState(false);
  const onCheck = async () => {
    setChecking(true);
    try {
      const update = await check();
      notify(update ? `发现新版本 ${update.version}，开始下载…` : '已是最新版本', 'info');
      if (update) {
        await update.downloadAndInstall();
        notify('更新已就绪，重启后生效', 'success');
      }
    } catch (e) {
      notify(`检查更新失败: ${e}`, 'error');
    } finally {
      setChecking(false);
    }
  };
  return (
    <div className="flex items-center gap-2">
      <span className="text-neutral-500">
        自动更新通道已配置（签名验证明文预留；发布源接入后生效）
      </span>
      <button
        className={`${inputCls} hover:border-blue-500`}
        onClick={() => void onCheck()}
        disabled={checking}
      >
        {checking ? '检查中…' : '检查更新'}
      </button>
    </div>
  );
}
