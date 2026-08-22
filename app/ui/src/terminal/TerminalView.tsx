import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { SerializeAddon } from '@xterm/addon-serialize';
import { useAppStore, type Pane, type Tab } from '../state/app-store';
import { termRegistry } from '../term/registry';

/**
 * 终端视图：xterm 生命周期 + 会话 attach。每个 pane 一份。
 * 显隐由外层 PaneFrame/tab 容器 display 控制——常驻挂载保留回滚与渲染状态；
 * ResizeObserver 在重新可见时自动 fit（0 尺寸跳过）。
 */
export function TerminalView({ tab, pane }: { tab: Tab; pane: Pane }) {
  const hostRef = useRef<HTMLDivElement>(null);
  const setPaneState = useAppStore((s) => s.setPaneState);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const term = new Terminal({
      scrollback: 10_000,
      allowProposedApi: true,
      fontFamily: "'Cascadia Code', 'JetBrains Mono', Consolas, monospace",
      fontSize: 14,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new SearchAddon());
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

    pane.session.attach(term, tab.spec).catch((e: unknown) => {
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
      term.dispose();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return <div ref={hostRef} className="h-full w-full p-1" />;
}
