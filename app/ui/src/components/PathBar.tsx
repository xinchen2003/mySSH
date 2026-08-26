import { useEffect, useRef, useState } from 'react';

/** 可编辑路径栏（批次五 11.1）：后退/前进/上一级/刷新 + 点击编辑。
 *  批次六：复制当前路径按钮；输入时基于父目录列表的模糊建议
 * （前缀/包含过滤，防抖 200ms，↑↓ 选择、Enter 采纳、Esc 先关建议再退出编辑）。
 *  历史由父组件持有；Enter 跳转失败时内联显示错误并保持编辑态。 */

interface Props {
  path: string;
  /** 空路径占位（本地盘符枚举 → 此电脑） */
  placeholder: string;
  loading: boolean;
  canBack: boolean;
  canFwd: boolean;
  onBack: () => void;
  onFwd: () => void;
  onUp: () => void;
  onRefresh: () => void;
  /** 用户输入跳转；resolve(true)=成功（退出编辑），false=失败（内联报错） */
  onNavigate: (path: string) => Promise<boolean>;
  /** 输入建议：按当前草稿拉父目录候选（失败/为空返回 []），缺省不启用 */
  fetchSuggestions?: (input: string) => Promise<string[]>;
}

/** 建议下拉条数上限 */
const MAX_SUGGESTIONS = 20;

export function PathBar(p: Props) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const editing = draft !== null;

  // 建议下拉（批次六）：仅编辑态启用；导航成功/取消编辑即关闭
  const [sugs, setSugs] = useState<string[]>([]);
  const [activeSug, setActiveSug] = useState(-1);
  const [sugOpen, setSugOpen] = useState(false);
  const sugTimer = useRef<number | null>(null);
  /** 已复制短暂反馈 */
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  // 卸载时清防抖定时器
  useEffect(
    () => () => {
      if (sugTimer.current !== null) window.clearTimeout(sugTimer.current);
    },
    [],
  );

  const closeSugs = () => {
    setSugOpen(false);
    setActiveSug(-1);
  };

  // 路径被外部改变（后退/前进/跟随终端）时取消编辑（渲染期派生重置，React 官方模式）
  const [lastPath, setLastPath] = useState(p.path);
  if (p.path !== lastPath) {
    setLastPath(p.path);
    setDraft(null);
    setError(false);
    closeSugs();
  }

  const scheduleSuggest = (value: string) => {
    const fetchSuggestions = p.fetchSuggestions;
    if (!fetchSuggestions) return;
    if (sugTimer.current !== null) window.clearTimeout(sugTimer.current);
    sugTimer.current = window.setTimeout(() => {
      void fetchSuggestions(value)
        .then((list) => {
          setSugs(list.slice(0, MAX_SUGGESTIONS));
          setSugOpen(list.length > 0);
          setActiveSug(-1);
        })
        .catch(() => closeSugs());
    }, 200);
  };

  const navigate = async (target: string): Promise<boolean> => {
    const ok = await p.onNavigate(target);
    if (ok) {
      setDraft(null);
      setError(false);
      closeSugs();
    }
    return ok;
  };

  const commit = async () => {
    if (draft === null) return;
    // 有选中建议：Enter 优先采纳建议
    if (sugOpen && activeSug >= 0 && sugs[activeSug]) {
      const target = sugs[activeSug];
      if (!(await navigate(target))) setError(true);
      return;
    }
    const target = draft.trim();
    if (!target || target === p.path) {
      setDraft(null);
      setError(false);
      closeSugs();
      return;
    }
    if (!(await navigate(target))) setError(true);
  };

  const copyPath = () => {
    if (!p.path) return;
    void navigator.clipboard.writeText(p.path).then(() => {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    });
  };

  const btn = 'rounded px-1 text-neutral-400 hover:bg-neutral-800 disabled:opacity-30';
  return (
    <div className="flex items-center gap-1 border-b border-neutral-800 px-2 py-1 text-xs text-neutral-500">
      <button className={btn} onClick={p.onBack} disabled={!p.canBack} title="后退">
        ←
      </button>
      <button className={btn} onClick={p.onFwd} disabled={!p.canFwd} title="前进">
        →
      </button>
      <button className={btn} onClick={p.onUp} title="上一级">
        ↑
      </button>
      <button className={btn} onClick={p.onRefresh} title="刷新">
        ⟳
      </button>
      {editing ? (
        <span className="relative flex min-w-0 flex-1 flex-col">
          <input
            ref={inputRef}
            className={`w-full rounded border bg-neutral-800 px-1.5 py-0.5 font-mono text-neutral-200 outline-none ${
              error ? 'border-red-600' : 'border-blue-600'
            }`}
            value={draft}
            onChange={(e) => {
              setDraft(e.target.value);
              setError(false);
              scheduleSuggest(e.target.value);
            }}
            onKeyDown={(e) => {
              if (e.key === 'ArrowDown' && sugOpen) {
                e.preventDefault();
                setActiveSug((i) => (i + 1) % sugs.length);
                return;
              }
              if (e.key === 'ArrowUp' && sugOpen) {
                e.preventDefault();
                setActiveSug((i) => (i <= 0 ? sugs.length - 1 : i - 1));
                return;
              }
              if (e.key === 'Enter') void commit();
              if (e.key === 'Escape') {
                // Esc 先关建议，再次 Esc 才退出编辑
                if (sugOpen) {
                  closeSugs();
                  return;
                }
                setDraft(null);
                setError(false);
              }
            }}
            onFocus={() => {
              if (sugs.length > 0) setSugOpen(true);
            }}
            onBlur={() => {
              // 点击建议项走 onMouseDown（先于 blur），此处直接收拢即可
              closeSugs();
              if (!error) setDraft(null);
            }}
          />
          {sugOpen && (
            <div className="absolute inset-x-0 top-full z-50 mt-0.5 max-h-48 overflow-y-auto rounded border border-neutral-700 bg-neutral-900 py-0.5 shadow-lg">
              {sugs.map((s, i) => (
                <div
                  key={s}
                  className={`cursor-pointer truncate px-2 py-0.5 font-mono ${
                    i === activeSug
                      ? 'bg-neutral-700 text-neutral-100'
                      : 'text-neutral-300 hover:bg-neutral-800'
                  }`}
                  onMouseDown={(e) => {
                    e.preventDefault(); // 先于 input blur 触发
                    void navigate(s).then((ok) => {
                      if (!ok) setError(true);
                    });
                  }}
                  onMouseEnter={() => setActiveSug(i)}
                >
                  {s}
                </div>
              ))}
            </div>
          )}
          {error && <span className="mt-0.5 text-red-400">路径无效或不可读</span>}
        </span>
      ) : (
        <span
          className="min-w-0 flex-1 cursor-text truncate rounded px-1 py-0.5 font-mono text-neutral-400 hover:bg-neutral-800"
          title={`${p.path || p.placeholder}（点击编辑）`}
          onClick={() => setDraft(p.path)}
        >
          {p.path || p.placeholder}
        </span>
      )}
      <button
        className={btn}
        onClick={copyPath}
        disabled={!p.path}
        title={copied ? '已复制' : '复制当前路径'}
      >
        {copied ? '✓' : '⧉'}
      </button>
      {p.loading && <span className="animate-pulse">…</span>}
    </div>
  );
}
