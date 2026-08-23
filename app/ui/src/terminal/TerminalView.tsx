import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { SerializeAddon } from '@xterm/addon-serialize';
import { readText, writeText } from '@tauri-apps/plugin-clipboard-manager';
import { useAppStore, type Pane, type Tab } from '../state/app-store';
import { fitRegistry, termRegistry } from '../term/registry';
import { resolveTheme } from '../term/themes';
import { readTerminalSettings } from '../state/apply-settings';
import { keymapFromSettings, matchCombo } from '../term/keymap';
import type { SessionStateFrame } from '../term/types';
import { SearchBar } from '../components/SearchBar';

/**
 * 终端视图：xterm 生命周期 + 会话 attach。每个 pane 一份。
 * 显隐由外层 PaneFrame/tab 容器 display 控制——常驻挂载保留回滚与渲染状态；
 * ResizeObserver 在重新可见时自动 fit（0 尺寸跳过）。
 *
 * 体验项（M1）：Ctrl+Shift+F 搜索浮条；选中即复制；右键粘贴（括号粘贴模式
 * 由 xterm 内建处理）；真彩色/Unicode11 宽字符/超链接由 addon 层提供。
 */
export function TerminalView({ tab, pane }: { tab: Tab; pane: Pane }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const setPaneState = useAppStore((s) => s.setPaneState);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const settings = useAppStore.getState().settings;
    const termOpts = readTerminalSettings(settings);
    const term = new Terminal({
      scrollback: termOpts.scrollback,
      allowProposedApi: true,
      fontFamily: termOpts.fontFamily,
      fontSize: termOpts.fontSize,
      theme: resolveTheme(
        typeof settings['theme'] === 'string' ? settings['theme'] : 'one-dark',
        typeof settings['theme.customJson'] === 'string' ? settings['theme.customJson'] : undefined,
      ).xterm,
    });
    const fit = new FitAddon();
    const search = new SearchAddon();
    searchRef.current = search;
    term.loadAddon(fit);
    term.loadAddon(search);
    term.loadAddon(new WebLinksAddon());
    term.loadAddon(new Unicode11Addon());
    term.loadAddon(new SerializeAddon());
    term.open(host);
    fit.fit();

    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // @xterm/addon-canvas 尚未支持 xterm 6（peer ^5），降级用 xterm 核心内置 canvas 渲染器
    }
    termRegistry.set(pane.id, term);
    fitRegistry.set(pane.id, () => {
      if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit();
    });

    // 搜索快捷键（keymap 注册表，默认 Ctrl+Shift+F）；其余按键全部放行给终端
    term.attachCustomKeyEventHandler((e) => {
      const bindings = keymapFromSettings(useAppStore.getState().settings);
      if (e.type === 'keydown' && bindings['search'] && matchCombo(e, bindings['search'])) {
        setSearchOpen(true);
        return false;
      }
      return true;
    });

    // 选中即复制（规格书 M1 体验项）。
    // 剪贴板走 Tauri 插件：WebView2 的 navigator.clipboard 在窗口无焦点时静默挂起（实测）。
    term.onSelectionChange(() => {
      const sel = term.getSelection();
      if (sel)
        void writeText(sel).catch((e: unknown) => {
          console.warn('copy failed', e);
        });
    });

    // 右键粘贴
    host.oncontextmenu = (e) => {
      e.preventDefault();
      void readText()
        .then((text) => {
          if (text) term.paste(text);
        })
        .catch((e: unknown) => {
          console.warn('paste failed', e);
        });
    };

    // 断线/重连/终态标记直接写入 xterm（同一实例续写，回滚天然保留）
    const stateHook = (ev: SessionStateFrame) => {
      if (ev.state === 'reconnecting')
        term.write(`\r\n\x1b[33m[连接中断，正在第 ${ev.attempt ?? '?'} 次重连…]\x1b[0m\r\n`);
      else if (ev.state === 'connected' && ev.reconnected)
        term.write('\x1b[32m[已重连]\x1b[0m\r\n');
      else if (ev.state === 'closed') term.write('\r\n\x1b[2m[连接已关闭]\x1b[0m\r\n');
    };

    pane.session.attach(term, tab.target, stateHook).catch((e: unknown) => {
      setPaneState(tab.id, pane.id, 'error');
      term.write(`\r\n\x1b[1;31m连接失败: ${String(e)}\x1b[0m\r\n`);
    });

    const observer = new ResizeObserver(() => {
      // display:none 时尺寸为 0，fit 会产生 0 列——跳过
      if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit();
    });
    observer.observe(host);
    return () => {
      observer.disconnect();
      termRegistry.delete(pane.id);
      fitRegistry.delete(pane.id);
      term.dispose();
      searchRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div className="relative h-full w-full">
      <div ref={hostRef} className="h-full w-full p-1" />
      {searchOpen && (
        <SearchBar
          onFind={(q, dir) => {
            if (!q) return;
            if (dir === 'next') searchRef.current?.findNext(q, { incremental: true });
            else searchRef.current?.findPrevious(q);
          }}
          onClose={() => setSearchOpen(false)}
        />
      )}
    </div>
  );
}
