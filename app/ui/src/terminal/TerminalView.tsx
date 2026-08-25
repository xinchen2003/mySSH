import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { SerializeAddon } from '@xterm/addon-serialize';
import { readText, writeText } from '@tauri-apps/plugin-clipboard-manager';
import { getCurrentWindow, UserAttentionType } from '@tauri-apps/api/window';
import { useAppStore, type Pane, type Tab } from '../state/app-store';
import { fitRegistry, reconnectRegistry, termRegistry } from '../term/registry';
import { resolveTheme } from '../term/themes';
import { readTerminalSettings } from '../state/apply-settings';
import { keymapFromSettings, matchAction, matchCombo } from '../term/keymap';
import type { SessionStateFrame } from '../term/types';
import { SearchBar, type SearchOptions, type SearchResults } from '../components/SearchBar';
import { ContextMenu, type MenuItem } from '../components/ContextMenu';

/**
 * 终端视图：xterm 生命周期 + 会话 attach。每个 pane 一份。
 * 显隐由外层 PaneFrame/tab 容器 display 控制——常驻挂载保留回滚与渲染状态；
 * ResizeObserver 在重新可见时自动 fit（0 尺寸跳过）。
 *
 * 体验项（M1）：Ctrl+Shift+F 搜索浮条；选中即复制；真彩色/Unicode11 宽字符/超链接
 * 由 addon 层提供。批次四：复制/粘贴快捷键（Ctrl+Shift+C/V、Ctrl/Shift+Insert，
 * Ctrl+C 保留给 SIGINT）；右键默认开菜单（可在设置改回直接粘贴）。
 */
export function TerminalView({ tab, pane }: { tab: Tab; pane: Pane }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<SearchAddon | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  /** 12.7：匹配计数（onDidChangeResults；decorations 开启才有全量统计） */
  const [searchResults, setSearchResults] = useState<SearchResults | null>(null);
  const setPaneState = useAppStore((s) => s.setPaneState);
  const closePane = useAppStore((s) => s.closePane);
  /** 终态原因（重连耗尽/连接失败）；非空时渲染非阻塞原位操作层 */
  const [dead, setDead] = useState<string | null>(null);
  /** 是否经历过自动重连（区分用户主动退出的 closed 与重连耗尽的 closed） */
  const sawReconnectRef = useRef(false);
  /** 挂载效应内注册的立即重连闭包（复用同一 xterm 与连接 target） */
  const reconnectRef = useRef<() => void>(() => undefined);
  /** 右键菜单（批次四 10.2）；canCopy 在打开瞬间采样，菜单存续期间不刷新 */
  const [menu, setMenu] = useState<{ x: number; y: number; canCopy: boolean } | null>(null);

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

    // 12.7：搜索结果统计（高亮 decorations 同时是计数来源）
    search.onDidChangeResults((r) => setSearchResults({ index: r.resultIndex, count: r.resultCount }));

    // 12.6 终端 bell：非活跃标签打标记（激活即清）；窗口非活动时闪任务栏。
    // 事件驱动，无定时器无动画；terminal.bell=false 全关
    term.onBell(() => {
      const s = useAppStore.getState();
      if (s.settings['terminal.bell'] === false) return;
      if (s.activeId !== tab.id) s.markBell(tab.id);
      if (!document.hasFocus()) {
        void getCurrentWindow()
          .requestUserAttention(UserAttentionType.Informational)
          .catch(() => undefined);
      }
    });

    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // @xterm/addon-canvas 尚未支持 xterm 6（peer ^5），降级用 xterm 核心内置 canvas 渲染器
    }
    termRegistry.set(pane.id, term);
    fitRegistry.set(pane.id, () => {
      if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit();
    });

    // 快捷键（keymap 注册表）：搜索/复制/粘贴在此拦截，其余按键全部放行给终端。
    // Ctrl+C 不在注册表内（保留给 SIGINT）；无选中时复制动作禁用（吞键不下发）。
    term.attachCustomKeyEventHandler((e) => {
      if (e.type !== 'keydown') return true;
      const bindings = keymapFromSettings(useAppStore.getState().settings);
      if (bindings['search'] && matchCombo(e, bindings['search'])) {
        setSearchOpen(true);
        return false;
      }
      if (matchAction(e, bindings, 'copy')) {
        if (term.hasSelection()) {
          void writeText(term.getSelection()).catch((err: unknown) => {
            console.warn('copy failed', err);
          });
        }
        return false;
      }
      if (matchAction(e, bindings, 'paste')) {
        void readText()
          .then((text) => {
            if (text) term.paste(text);
          })
          .catch((err: unknown) => {
            console.warn('paste failed', err);
          });
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

    // 右键（批次四 10.2）：默认开菜单；terminal.rightClickPaste=true 恢复直接粘贴
    host.oncontextmenu = (e) => {
      e.preventDefault();
      const s = useAppStore.getState();
      s.setActivePane(tab.id, pane.id);
      if (s.settings['terminal.rightClickPaste'] === true) {
        void readText()
          .then((text) => {
            if (text) term.paste(text);
          })
          .catch((err: unknown) => {
            console.warn('paste failed', err);
          });
        return;
      }
      setMenu({ x: e.clientX, y: e.clientY, canCopy: term.hasSelection() });
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
        if (sawReconnectRef.current) {
          const msg = '连接中断，自动重连已耗尽（5 次尝试）';
          setDead(msg);
          if (tab.target.kind === 'session')
            useAppStore.getState().recordConnectFailure(tab.target.sessionId, msg);
        }
      }
    };

    const attachNow = () => {
      pane.session.attach(term, tab.target, stateHook).catch((e: unknown) => {
        setPaneState(tab.id, pane.id, 'error');
        const msg = `连接失败: ${String(e)}`;
        setDead(msg);
        if (tab.target.kind === 'session')
          useAppStore.getState().recordConnectFailure(tab.target.sessionId, msg);

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
    reconnectRegistry.set(pane.id, () => reconnectRef.current());
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
      reconnectRegistry.delete(pane.id);
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
          results={searchResults}
          onFind={(q, dir, opts: SearchOptions) => {
            if (!q) {
              searchRef.current?.clearDecorations();
              setSearchResults(null);
              return;
            }
            // decorations 是 onDidChangeResults 计数的前提；颜色取语义中性
            const o = {
              caseSensitive: opts.caseSensitive,
              wholeWord: opts.wholeWord,
              decorations: {
                matchBackground: '#4b5563',
                matchOverviewRuler: '#9ca3af',
                activeMatchBackground: '#2563eb',
                activeMatchColorOverviewRuler: '#60a5fa',
              },
            };
            if (dir === 'next') searchRef.current?.findNext(q, { ...o, incremental: true });
            else searchRef.current?.findPrevious(q, o);
          }}
          onClose={() => {
            setSearchOpen(false);
            searchRef.current?.clearDecorations();
            setSearchResults(null);
          }}
        />
      )}
      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          onClose={() => setMenu(null)}
          items={termMenuItems(menu.canCopy)}
        />
      )}
    </div>
  );

  /** 终端右键菜单（批次四 10.2）。xterm 实例经 registry 取（menu 打开时必然已挂载） */
  function termMenuItems(canCopy: boolean): MenuItem[] {
    const term = termRegistry.get(pane.id);
    const s = useAppStore.getState();
    const isSession = tab.target.kind === 'session';
    const paste = () =>
      void readText()
        .then((t) => {
          if (t) term?.paste(t);
        })
        .catch((e: unknown) => console.warn('paste failed', e));
    return [
      { label: '粘贴', onSelect: paste },
      {
        label: '复制',
        disabled: !canCopy,
        onSelect: () => {
          const sel = term?.getSelection();
          if (sel)
            void writeText(sel).catch((e: unknown) => {
              console.warn('copy failed', e);
            });
        },
      },
      { label: '全选', onSelect: () => term?.selectAll() },
      { label: '搜索', onSelect: () => setSearchOpen(true) },
      'separator',
      { label: '清空屏幕', onSelect: () => term?.write('\x1b[2J\x1b[H') },
      { label: '清空回滚', onSelect: () => term?.write('\x1b[3J') },
      'separator',
      { label: '向右分屏', onSelect: () => s.splitActive('row') },
      { label: '向下分屏', onSelect: () => s.splitActive('col') },
      'separator',
      { label: '重新连接', onSelect: () => reconnectRef.current() },
      {
        label: '打开 SFTP',
        disabled: !isSession,
        onSelect: () => {
          if (!s.sftpOpen[tab.id]) s.toggleSftp(tab.id);
        },
      },
    ];
  }
}
