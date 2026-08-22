import { useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';

/**
 * 会话档案侧栏：按 groupPath 分组展示；点击直连（凭据从保险库解析）。
 * 悬停出现 编辑/删除。空态引导新建。
 */
export function Sidebar() {
  const sessions = useAppStore((s) => s.sessions);
  const open = useAppStore((s) => s.sidebarOpen);
  const load = useAppStore((s) => s.loadSessions);
  const connectBySession = useAppStore((s) => s.connectBySession);
  const deleteSession = useAppStore((s) => s.deleteSession);
  const openConnect = useAppStore((s) => s.openConnect);

  useEffect(() => {
    void load();
  }, [load]);

  if (!open) return null;

  // groupPath 分组（'' 归"未分组"）
  const groups = new Map<string, typeof sessions>();
  for (const s of sessions) {
    const g = s.groupPath || '未分组';
    groups.set(g, [...(groups.get(g) ?? []), s]);
  }

  return (
    <aside className="flex w-56 shrink-0 flex-col border-r border-neutral-800 bg-neutral-900">
      <div className="flex items-center justify-between px-3 py-2 text-xs text-neutral-400">
        <span>会话</span>
        <span className="flex gap-1">
          <button
            className="rounded px-1 hover:bg-neutral-800 hover:text-neutral-100"
            onClick={() => {
              void invoke<{ imported: number; skipped: number }>('import_openssh', {})
                .then((r) => window.alert(`导入完成：新增/更新 ${r.imported}，跳过 ${r.skipped}`))
                .then(() => load())
                .catch((e: unknown) => window.alert(`导入失败：${String(e)}`));
            }}
            title="从 ~/.ssh/config 导入"
          >
            ⇩
          </button>
          <button
            className="rounded px-1 hover:bg-neutral-800 hover:text-neutral-100"
            onClick={() => openConnect()}
            title="新建会话"
          ></button>
        </span>
      </div>
      <div className="flex-1 overflow-y-auto px-1 pb-2">
        {sessions.length === 0 && (
          <p className="px-2 py-4 text-center text-xs text-neutral-600">
            暂无保存的会话
            <br />
            新建连接时勾选「保存会话」
          </p>
        )}
        {[...groups.entries()].map(([group, items]) => (
          <div key={group} className="mt-1">
            <div className="px-2 py-0.5 text-[10px] tracking-wide text-neutral-600">{group}</div>
            {items.map((s) => (
              <div
                key={s.id}
                className="group flex cursor-pointer items-center justify-between rounded px-2 py-1 text-xs text-neutral-300 hover:bg-neutral-800"
                onClick={() => connectBySession(s.id, `${s.username}@${s.host}`)}
                title={`${s.username}@${s.host}:${s.port} · ${s.authType}`}
              >
                <span className="truncate">{s.name}</span>
                <span className="hidden shrink-0 gap-1 group-hover:flex">
                  <button
                    className="text-neutral-500 hover:text-neutral-200"
                    onClick={(e) => {
                      e.stopPropagation();
                      openConnect(s);
                    }}
                    title="编辑"
                  >
                    ✎
                  </button>
                  <button
                    className="text-neutral-500 hover:text-red-400"
                    onClick={(e) => {
                      e.stopPropagation();
                      if (window.confirm(`删除会话「${s.name}」？（凭据一并清除）`))
                        void deleteSession(s.id);
                    }}
                    title="删除"
                  >
                    ×
                  </button>
                </span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </aside>
  );
}
