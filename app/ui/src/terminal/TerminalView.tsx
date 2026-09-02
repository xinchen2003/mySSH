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
import { isLocalTarget, useAppStore, type Pane, type Tab } from '../state/app-store';
import { fitRegistry, reconnectRegistry, termRegistry } from '../term/registry';
import { resolveTheme } from '../term/themes';
import {
  effectiveXtermTheme,
  readTermBackground,
  readTerminalSettings,
} from '../state/apply-settings';
import { keymapFromSettings, matchAction, matchCombo } from '../term/keymap';
import type { SessionStateFrame } from '../term/types';
import { SearchBar, type SearchOptions, type SearchResults } from '../components/SearchBar';
import { ContextMenu, type MenuItem } from '../components/ContextMenu';
import { ConfirmDialog } from '../components/ConfirmDialog';
import { useTransferStore } from '../state/transfer-store';
import { useT, tNow } from '../i18n';
/**
 * pane 终端运行时（条目 6a）：跨布局变化（leaf↔split 重挂）保活的 xterm 实例。
 * 分屏/合屏改变 SplitTree 的嵌套结构 → TerminalView 必卸载重挂；
 * 卸载时若 pane 仍在 store（panes 表未移除）则 runtime 留池复用，新挂载把同一
 * term.element DOM 移入新宿主——滚动缓冲、渲染态、SSH 通道全部原样保留；
 * pane 真正关闭（panes 表移除）时才 dispose。
 */
interface PaneRuntime {
  term: Terminal;
  fit: FitAddon;
  search: SearchAddon;
  /** 是否经历过自动重连（区分用户主动退出的 closed 与重连耗尽的 closed） */
  sawReconnect: boolean;
  /** 终态原因缓存：重挂后新实例以此为初值，原位操作层不丢 */
  deadMsg: string | null;
  /** session_state 旁路钩子：经 ui 出口动态触达当前挂载实例（旧闭包重挂后仍指最新 setState） */
  stateHook: (ev: SessionStateFrame) => void;
  /** 最近一次挂载写入的组件态出口 */
  ui: {
    setSearchOpen: (open: boolean) => void;
    setSearchResults: (r: SearchResults | null) => void;
    setDead: (msg: string | null) => void;
    setMenu: (m: { x: number; y: number; canCopy: boolean } | null) => void;
  };
}

/** pane id → 终端运行时保活池 */
const paneRuntimes = new Map<string, PaneRuntime>();

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
  /** 终态原因（重连耗尽/连接失败）；非空时渲染非阻塞原位操作层。初值取保活池缓存（重挂恢复） */
  const [dead, setDead] = useState<string | null>(() => paneRuntimes.get(pane.id)?.deadMsg ?? null);
  /** 挂载效应内注册的立即重连闭包（复用同一 xterm 与连接 target） */
  const reconnectRef = useRef<() => void>(() => undefined);
  /** 右键菜单（批次四 10.2）；canCopy 在打开瞬间采样，菜单存续期间不刷新 */
  const [menu, setMenu] = useState<{ x: number; y: number; canCopy: boolean } | null>(null);
  /** 多行粘贴确认（批次十一）：非空时渲染确认框，text 为待粘贴原文 */
  const [pasteConfirm, setPasteConfirm] = useState<{ text: string; lines: number } | null>(null);
  const t = useT();

  /** 统一粘贴入口（快捷键/右键直贴/菜单项三路共用）：去除尾部单个换行后仍含换行 → 确认；
   *  单行直贴。确认框默认焦点在「取消」（ConfirmDialog 语义），防误回车批量执行命令 */
  const confirmPaste = (text: string) => {
    const stripped = text.replace(/\r?\n$/, '');
    if (stripped.includes('\n')) {
      setPasteConfirm({ text, lines: stripped.split('\n').length });
      return;
    }
    termRegistry.get(pane.id)?.paste(text);
  };

  /** 统一读剪贴板（三条粘贴路径共用）：读取失败（如剪贴板被其他程序占用）toast 提示，
   *  不再只写 console——用户按了粘贴没反应时必须知道原因 */
  const readClipboard = () => {
    void readText()
      .then((text) => {
        if (text) confirmPaste(text);
      })
      .catch(() => {
        useAppStore.getState().notify(tNow('state.clipboardReadFailed'), 'error');
      });
  };

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    /** 建立（或原位重连）SSH 通道；attach 幂等防重复订阅 */
    const attachNow = (r: PaneRuntime) => {
      pane.session.attach(r.term, tab.target, r.stateHook).catch((e: unknown) => {
        setPaneState(tab.id, pane.id, 'error');
        const msg = tNow('state.connectFailed', { error: String(e) });
        r.ui.setDead(msg);
        if (tab.target.kind === 'session')
          useAppStore.getState().recordConnectFailure(tab.target.sessionId, msg);

        r.term.write(`\r\n\x1b[1;31m${msg}\x1b[0m\r\n`);
      });
    };

    /** setDead 同步写 runtime 缓存：重挂后新实例可恢复原位操作层 */
    const uiOf = (r: PaneRuntime): PaneRuntime['ui'] => ({
      setSearchOpen,
      setSearchResults,
      setDead: (m) => {
        r.deadMsg = m;
        setDead(m);
      },
      setMenu,
    });

    let rt = paneRuntimes.get(pane.id);
    if (rt) {
      // 布局重排（leaf↔split）重挂：xterm 实例/滚动缓冲/SSH 通道全部保活，
      // 只把同一 DOM 节点移入新宿主并刷新组件态出口；session 输入订阅仍在，不重复 attach
      rt.ui = uiOf(rt);
      if (rt.term.element && rt.term.element.parentElement !== host)
        host.appendChild(rt.term.element);
      searchRef.current = rt.search;
    } else {
      const settings = useAppStore.getState().settings;
      const termOpts = readTerminalSettings(settings);
      // 批次十一 8：断线重连次数可配（terminal.reconnectAttempts，0-20 默认 5；挂载时读一次）
      const rcRaw = settings['terminal.reconnectAttempts'];
      const reconnectAttempts =
        typeof rcRaw === 'number' && rcRaw >= 0 && rcRaw <= 20 ? Math.trunc(rcRaw) : 5;
      const term = new Terminal({
        scrollback: termOpts.scrollback,
        allowProposedApi: true,
        fontFamily: termOpts.fontFamily,
        fontSize: termOpts.fontSize,
        // 背景图模式需透明背景（allowTransparency 只允许构造期设置，恒开无开销）
        allowTransparency: true,
        theme: effectiveXtermTheme(
          settings,
          resolveTheme(
            typeof settings['theme'] === 'string' ? settings['theme'] : 'one-dark',
            typeof settings['theme.customJson'] === 'string'
              ? settings['theme.customJson']
              : undefined,
          ),
        ),
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

      const rtNew: PaneRuntime = {
        term,
        fit,
        search,
        sawReconnect: false,
        deadMsg: null,
        ui: { setSearchOpen, setSearchResults, setDead, setMenu },
        // 断线/重连/终态标记直接写入 xterm（同一实例续写，回滚天然保留）；
        // 组件态经 rtNew.ui 出口——重挂后此闭包仍指向最新挂载实例
        stateHook: (ev) => {
          if (ev.state === 'reconnecting') {
            rtNew.sawReconnect = true;
            rtNew.term.write(
              `\r\n\x1b[33m${tNow('state.reconnecting', { attempt: ev.attempt ?? '?' })}\x1b[0m\r\n`,
            );
          } else if (ev.state === 'connected') {
            rtNew.sawReconnect = false;
            rtNew.ui.setDead(null);
            if (ev.reconnected) rtNew.term.write(`\x1b[32m${tNow('state.reconnected')}\x1b[0m\r\n`);
          } else if (ev.state === 'closed') {
            rtNew.term.write(`\r\n\x1b[2m${tNow('state.connClosed')}\x1b[0m\r\n`);
            // 重连耗尽（后端发 closed 终态）→ 原位操作层；用户主动 exit 的关闭不打扰
            if (rtNew.sawReconnect) {
              const msg = tNow('state.reconnectExhausted', { count: reconnectAttempts });
              rtNew.ui.setDead(msg);
              if (tab.target.kind === 'session')
                useAppStore.getState().recordConnectFailure(tab.target.sessionId, msg);
            }
          }
        },
      };
      rt = rtNew;
      paneRuntimes.set(pane.id, rtNew);
      rtNew.ui = uiOf(rtNew);

      // 12.7：搜索结果统计（高亮 decorations 同时是计数来源）
      search.onDidChangeResults((r) =>
        rtNew.ui.setSearchResults({ index: r.resultIndex, count: r.resultCount }),
      );

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

      // WebGL 渲染器不支持透明背景：配置了终端背景图时跳过加载，用内置 canvas 渲染器
      if (readTermBackground(settings).image) {
        // 背景图模式：内置 canvas 渲染（支持 allowTransparency）
      } else {
        try {
          term.loadAddon(new WebglAddon());
        } catch {
          // @xterm/addon-canvas 尚未支持 xterm 6（peer ^5），降级用 xterm 核心内置 canvas 渲染器
        }
      }

      // 快捷键（keymap 注册表）：搜索/复制/粘贴在此拦截，其余按键全部放行给终端。
      // Ctrl+C 不在注册表内（保留给 SIGINT）；无选中时复制动作禁用（吞键不下发）。
      term.attachCustomKeyEventHandler((e) => {
        if (e.type !== 'keydown') return true;
        const bindings = keymapFromSettings(useAppStore.getState().settings);
        if (bindings['search'] && matchCombo(e, bindings['search'])) {
          rtNew.ui.setSearchOpen(true);
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
          readClipboard();
          return false;
        }
        // 批次十一：全局动作（App.tsx window 处理器执行）在此吞键防落入远端——
        // 返回 false 只挡 xterm 内部处理，DOM 事件照常冒泡到全局快捷键处理器
        if (
          matchAction(e, bindings, 'zoomIn') ||
          matchAction(e, bindings, 'zoomOut') ||
          matchAction(e, bindings, 'resetZoom') ||
          matchAction(e, bindings, 'nextPane') ||
          matchAction(e, bindings, 'reopenClosedTab') ||
          (e.ctrlKey && !e.altKey && !e.shiftKey && /^[1-9]$/.test(e.key))
        ) {
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

      // 首挂建立会话（重挂跳过：session 输入订阅与后端通道仍在）
      attachNow(rtNew);
    }

    // —— 以下为每次挂载都需重建的宿主相关接线（host 元素随重挂更换） ——
    // const 固定 narrowing：let 在闭包内会回退到声明类型（含 undefined）
    const runtime = rt;
    const { term, fit } = runtime;
    termRegistry.set(pane.id, term);
    fitRegistry.set(pane.id, () => {
      if (host.clientWidth > 0 && host.clientHeight > 0) fit.fit();
    });

    // 右键（批次四 10.2）：默认开菜单；terminal.rightClickPaste=true 恢复直接粘贴
    host.oncontextmenu = (e) => {
      e.preventDefault();
      const s = useAppStore.getState();
      s.setActivePane(tab.id, pane.id);
      if (s.settings['terminal.rightClickPaste'] === true) {
        readClipboard();
        return;
      }
      runtime.ui.setMenu({ x: e.clientX, y: e.clientY, canCopy: term.hasSelection() });
    };

    // 原位重连：复用当前 target 与 xterm（回滚保留、不新建标签）
    reconnectRef.current = () => {
      runtime.sawReconnect = false;
      runtime.ui.setDead(null);
      setPaneState(tab.id, pane.id, 'connecting');
      term.write(`\r\n\x1b[33m${tNow('state.reconnectingNow')}\x1b[0m\r\n`);
      attachNow(runtime);
    };
    reconnectRegistry.set(pane.id, () => reconnectRef.current());

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
      searchRef.current = null;
      // pane 仍在 store = 布局重排重挂 → runtime 留池保活等复用；
      // 已从 panes 表移除 = 真关闭（session 由 closePane/doCloseTab 关闭），dispose
      const alive = useAppStore.getState().tabs.some((t) => t.panes[pane.id] !== undefined);
      if (!alive) {
        paneRuntimes.delete(pane.id);
        term.dispose();
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      className="relative h-full w-full"
      onWheel={(e) => {
        // 批次十一 3：Ctrl+滚轮字号缩放（与 keymap zoomIn/zoomOut 同一 settings 通道，钳 8-32）
        if (!e.ctrlKey) return;
        e.preventDefault();
        const s = useAppStore.getState();
        const cur = readTerminalSettings(s.settings).fontSize;
        const next = Math.min(32, Math.max(8, cur + (e.deltaY < 0 ? 1 : -1)));
        if (next !== cur) s.setSetting('terminal.fontSize', next);
      }}
    >
      <div ref={hostRef} className="myssh-term-host relative h-full w-full p-1" />
      {dead && (
        <div className="absolute bottom-2 left-1/2 z-10 flex max-w-[95%] -translate-x-1/2 items-center gap-2 rounded border border-neutral-700 bg-neutral-900/95 px-3 py-1.5 text-xs shadow-lg">
          <span className="min-w-0 max-w-64 truncate text-red-300" title={dead}>
            {dead}
          </span>
          <button
            className="shrink-0 rounded bg-blue-600 px-2 py-0.5 text-white hover:bg-blue-500"
            onClick={() => reconnectRef.current()}
          >
            {t('state.reconnectNow')}
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
                else s.notify(tNow('state.sessionNotFound'), 'warning');
              }}
            >
              {t('state.editConnection')}
            </button>
          )}
          <button
            className="shrink-0 rounded px-2 py-0.5 text-neutral-400 hover:bg-neutral-800"
            onClick={() => closePane(tab.id, pane.id)}
          >
            {t('state.close')}
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
      {/* 批次十一 1：多行粘贴确认（默认焦点在取消，防误回车执行多条命令） */}
      {pasteConfirm && (
        <ConfirmDialog
          title={t('state.pasteConfirmTitle')}
          confirmLabel={t('state.key.paste')}
          onCancel={() => setPasteConfirm(null)}
          onConfirm={() => {
            termRegistry.get(pane.id)?.paste(pasteConfirm.text);
            setPasteConfirm(null);
          }}
        >
          <p className="mb-1">{t('state.pasteConfirmBody', { lines: pasteConfirm.lines })}</p>
          <p className="text-red-300">{t('state.pasteConfirmWarn')}</p>
        </ConfirmDialog>
      )}
    </div>
  );

  /** 终端右键菜单（批次四 10.2）。xterm 实例经 registry 取（menu 打开时必然已挂载） */
  function termMenuItems(canCopy: boolean): MenuItem[] {
    const term = termRegistry.get(pane.id);
    const s = useAppStore.getState();
    const isSession = tab.target.kind === 'session';
    const isRemote = isSession && !isLocalTarget(tab.target);
    const paste = () => readClipboard();
    return [
      { label: t('state.key.paste'), icon: '⤵', onSelect: paste },
      {
        label: t('state.menu.copy'),
        icon: '⧉',
        disabled: !canCopy,
        onSelect: () => {
          const sel = term?.getSelection();
          if (sel)
            void writeText(sel).catch((e: unknown) => {
              console.warn('copy failed', e);
            });
        },
      },
      { label: t('state.menu.selectAll'), icon: '□', onSelect: () => term?.selectAll() },
      { label: t('state.menu.search'), icon: '⌕', onSelect: () => setSearchOpen(true) },
      'separator',
      {
        label: t('state.menu.clearScreen'),
        icon: '⌫',
        onSelect: () => term?.write('\x1b[2J\x1b[H'),
      },
      { label: t('state.menu.clearScrollback'), icon: '⌫', onSelect: () => term?.write('\x1b[3J') },
      'separator',
      { label: t('state.key.splitRow'), onSelect: () => s.splitActive('row') },
      { label: t('state.key.splitCol'), onSelect: () => s.splitActive('col') },
      'separator',
      { label: t('state.menu.reconnect'), icon: '▶', onSelect: () => reconnectRef.current() },
      {
        label: t('state.menu.openSftp'),
        icon: '⇅',
        disabled: !isRemote,
        onSelect: () => {
          s.openDock('sftp');
          // 面板打开后定位到终端 cwd（SftpPanel 消费 navRequests，不入历史栈）
          const cwd = pane.session.cwd;
          if (cwd) useTransferStore.getState().requestNav(tab.id, cwd);
        },
      },
    ];
  }
}
