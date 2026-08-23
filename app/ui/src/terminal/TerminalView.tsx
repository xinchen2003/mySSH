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
  const closePane = useAppStore((s) => s.closePane);
  /** 终态原因（重连耗尽/连接失败）；非空时渲染非阻塞原位操作层 */
  const [dead, setDead] = useState<string | null>(null);
  /** 是否经历过自动重连（区分用户主动退出的 closed 与重连耗尽的 closed） */
  const sawReconnectRef = useRef(false);
  /** 挂载效应内注册的立即重连闭包（复用同一 xterm 与连接 target） */
  const reconnectRef = useRef<() => void>(() => undefined);

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
      // 批次一 7.5：可开关；事件时读设置 → 修改对已有终端立即生效
      if (useAppStore.getState().settings['terminal.copyOnSelect'] === false) return;
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
      if (ev.state === 'reconnecting') {
        sawReconnectRef.current = true;
        term.write(`\r\n\x1b[33m[连接中断，正在第 ${ev.attempt ?? '?'} 次重连…]\x1b[0m\r\n`);
      } else if (ev.state === 'connected') {
        sawReconnectRef.current = false;
        setDead(null);
        if (ev.reconnected) term.write('\x1b[32m[已重连]\x1b[0m\r\n');
      } else if (ev.state === 'closed') {
        term.write('\r\n\x1b[2m[连接已关闭]\x1b[0m\r\n');
        // 重连耗尽（后端发 closed 终态）→ 原位操作层；用户主动 exit 的关闭不打扰
        if (sawReconnectRef.current) setDead('连接中断，自动重连已耗尽（5 次尝试）');
      }
    };

    const attachNow = () => {
      pane.session.attach(term, tab.target, stateHook).catch((e: unknown) => {
        setPaneState(tab.id, pane.id, 'error');
        const msg = `连接失败: ${String(e)}`;
        setDead(msg);
        term.write(`\r\n\x1b[1;31m${msg}\x1b[0m\r\n`);
      });
    };
    // 原位重连：复用当前 target 与 xterm（回滚保留、不新建标签；attach 幂等防重复订阅）
    reconnectRef.current = () => {
      sawReconnectRef.current = false;
      setDead(null);
      setPaneState(tab.id, pane.id, 'connecting');
      term.write('\r\n\x1b[33m[正在重新连接…]\x1b[0m\r\n');
      attachNow();
    };
    attachNow();

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
      {dead && (
        <div className="absolute bottom-2 left-1/2 z-10 flex max-w-[95%] -translate-x-1/2 items-center gap-2 rounded border border-neutral-700 bg-neutral-900/95 px-3 py-1.5 text-xs shadow-lg">
          <span className="min-w-0 max-w-64 truncate text-red-300" title={dead}>
            {dead}
          </span>
          <button
            className="shrink-0 rounded bg-blue-600 px-2 py-0.5 text-white hover:bg-blue-500"
            onClick={() => reconnectRef.current()}
          >
            立即重连
          </button>
          {tab.target.kind === 'session' && (
            <button
              className="shrink-0 rounded px-2 py-0.5 text-neutral-300 hover:bg-neutral-800"
              onClick={() => {
                const t = tab.target;
                if (t.kind !== 'session') return;
                const s = useAppStore.getState();
                const rec = s.sessions.find((r) => r.id === t.sessionId);
                if (rec) s.openConnect(rec);
                else s.notify('未找到会话档案，可能已被删除', 'warning');
              }}
            >
              编辑连接
            </button>
          )}
          <button
            className="shrink-0 rounded px-2 py-0.5 text-neutral-400 hover:bg-neutral-800"
            onClick={() => closePane(tab.id, pane.id)}
          >
            关闭
          </button>
        </div>
      )}
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
