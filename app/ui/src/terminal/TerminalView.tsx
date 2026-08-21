import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebglAddon } from '@xterm/addon-webgl';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { SerializeAddon } from '@xterm/addon-serialize';

/**
 * 终端视图。M0：xterm + 全 addon 挂载 + WebGL→Canvas 降级路径。
 * 数据通路（term_open/StreamConsumer 接线）在 M1 接通。
 */
export function TerminalView() {
  const hostRef = useRef<HTMLDivElement>(null);

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

    let renderer = 'webgl';
    try {
      term.loadAddon(new WebglAddon());
    } catch {
      // @xterm/addon-canvas 尚未支持 xterm 6（peer ^5），降级用 xterm 核心内置 canvas 渲染器
      renderer = 'canvas(core)';
    }
    term.write(`\x1b[1;36mmySSH\x1b[0m M0 骨架 · 渲染器 ${renderer}\r\n`);
    term.write(
      '\x1b[2m终端链路：8ms/256KB 聚合 → ipc::Channel(Raw) → rAF 背压 → credit 闸门\x1b[0m\r\n',
    );

    const observer = new ResizeObserver(() => fit.fit());
    observer.observe(host);
    return () => {
      observer.disconnect();
      term.dispose();
    };
  }, []);

  return <div ref={hostRef} className="h-full w-full p-1" />;
}
