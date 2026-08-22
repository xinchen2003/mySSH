import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';

/**
 * 主机密钥确认弹窗（安全模型第 3 条）：
 * 首连展示指纹，变更时硬警告（红）并展示新旧指纹对比。
 */
export function HostKeyDialog() {
  const pending = useAppStore((s) => s.pendingHostKeys[0] ?? null);
  const clear = useAppStore((s) => s.shiftHostKey);

  if (!pending) return null;
  const changed = pending.kind === 'changed';

  const answer = (accept: boolean, remember: boolean) => {
    void invoke('hostkey_confirm', {
      confirmId: pending.confirmId,
      accept,
      remember,
    });
    clear();
  };

  return (
    <div className="fixed inset-0 z-20 flex items-center justify-center bg-black/60">
      <div
        className={`w-[28rem] rounded-lg border p-4 text-sm shadow-xl ${
          changed
            ? 'border-red-700 bg-red-950 text-red-100'
            : 'border-neutral-700 bg-neutral-900 text-neutral-200'
        }`}
      >
        {changed ? (
          <>
            <h2 className="mb-2 text-base font-semibold text-red-300">
              ⚠ 主机密钥已变更——可能存在中间人攻击
            </h2>
            <p className="mb-2 text-xs">
              {pending.host}:{pending.port}（{pending.keyType}）
            </p>
            <div className="mb-3 flex flex-col gap-1 rounded bg-black/40 p-2 font-mono text-xs">
              <span>旧：{pending.oldFingerprint}</span>
              <span className="text-red-300">新：{pending.newFingerprint}</span>
            </div>
            <div className="flex justify-end gap-2">
              <button
                className="rounded bg-neutral-700 px-3 py-1 hover:bg-neutral-600"
                onClick={() => answer(false, false)}
              >
                拒绝连接
              </button>
              <button
                className="rounded bg-red-600 px-3 py-1 text-white hover:bg-red-500"
                onClick={() => answer(true, true)}
              >
                我已核实，更新记录并连接
              </button>
            </div>
          </>
        ) : (
          <>
            <h2 className="mb-2 text-base font-semibold">首次连接：确认主机指纹</h2>
            <p className="mb-2 text-xs text-neutral-400">
              {pending.host}:{pending.port}（{pending.keyType}）
            </p>
            <div className="mb-3 rounded bg-black/40 p-2 font-mono text-xs">
              {pending.fingerprint}
            </div>
            <div className="flex justify-end gap-2">
              <button
                className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
                onClick={() => answer(false, false)}
              >
                拒绝
              </button>
              <button
                className="rounded border border-neutral-600 px-3 py-1 hover:bg-neutral-800"
                onClick={() => answer(true, false)}
              >
                仅本次信任
              </button>
              <button
                className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500"
                onClick={() => answer(true, true)}
              >
                信任并记住
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
