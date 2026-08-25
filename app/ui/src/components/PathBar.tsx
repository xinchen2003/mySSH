import { useEffect, useRef, useState } from 'react';

/** 可编辑路径栏（批次五 11.1）：后退/前进/上一级/刷新 + 点击编辑。
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
}

export function PathBar(p: Props) {
  const [draft, setDraft] = useState<string | null>(null);
  const [error, setError] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const editing = draft !== null;

  useEffect(() => {
    if (editing) inputRef.current?.select();
  }, [editing]);

  // 路径被外部改变（后退/前进/跟随终端）时取消编辑
  const [lastPath, setLastPath] = useState(p.path);
  // 路径被外部改变（后退/前进/跟随终端）时取消编辑（渲染期派生重置，React 官方模式）
  if (p.path !== lastPath) {
    setLastPath(p.path);
    setDraft(null);
    setError(false);
  }

  const commit = async () => {
    if (draft === null) return;
    const target = draft.trim();
    if (!target || target === p.path) {
      setDraft(null);
      setError(false);
      return;
    }
    const ok = await p.onNavigate(target);
    if (ok) {
      setDraft(null);
      setError(false);
    } else {
      setError(true);
    }
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
        <span className="flex min-w-0 flex-1 flex-col">
          <input
            ref={inputRef}
            className={`w-full rounded border bg-neutral-800 px-1.5 py-0.5 font-mono text-neutral-200 outline-none ${
              error ? 'border-red-600' : 'border-blue-600'
            }`}
            value={draft}
            onChange={(e) => {
              setDraft(e.target.value);
              setError(false);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void commit();
              if (e.key === 'Escape') {
                setDraft(null);
                setError(false);
              }
            }}
            onBlur={() => {
              if (!error) setDraft(null);
            }}
          />
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
      {p.loading && <span className="animate-pulse">…</span>}
    </div>
  );
}
