import { createRoot } from 'react-dom/client';
import { App } from './App';
import { installDevHooks } from './dev';
import './index.css';

const rootEl = document.getElementById('root');
if (!rootEl) {
  throw new Error('missing #root');
}

// 不用 StrictMode：其开发期双挂载会让 TerminalView 的 attach（term_open）触发两次
installDevHooks();
createRoot(rootEl).render(<App />);
