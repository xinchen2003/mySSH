import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import type { KiChallengeFrame } from '../term/types';

/**
 * keyboard-interactive（2FA）逐轮应答弹窗。
 * echo=false 的提问用密码输入框（不回显）。
 */
export function KiDialog() {
  const pending = useAppStore((s) => s.pendingKis[0] ?? null);
  if (!pending) return null;
  // key=confirmId：新一轮质询重新挂载表单，答案数组随之重置
  return <KiForm key={pending.confirmId} pending={pending} />;
}

function KiForm({ pending }: { pending: KiChallengeFrame }) {
  const clear = useAppStore((s) => s.shiftKi);
  const [answers, setAnswers] = useState<string[]>(() => pending.prompts.map(() => ''));

  const submit = (values: string[] | null) => {
    void invoke('ki_respond', { confirmId: pending.confirmId, answers: values });
    clear();
  };

  return (
    <div
      className="fixed inset-0 z-20 flex items-center justify-center bg-black/60"
      role="dialog"
      aria-modal="true"
      aria-label="键盘交互认证"
    >
      <div className="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl">
        <h2 className="mb-1 text-base font-semibold">二次认证</h2>
        {pending.name && <p className="text-xs text-neutral-400">{pending.name}</p>}
        {pending.instruction && (
          <p className="mb-2 text-xs text-neutral-400">{pending.instruction}</p>
        )}
        <div className="mt-2 flex flex-col gap-2">
          {pending.prompts.map((p, i) => (
            <label key={i}>
              <span className="mb-0.5 block text-xs text-neutral-400">{p.prompt}</span>
              <input
                className="w-full rounded border border-neutral-700 bg-neutral-800 px-2 py-1"
                type={p.echo ? 'text' : 'password'}
                value={answers[i] ?? ''}
                onChange={(e) =>
                  setAnswers((prev) => prev.map((a, j) => (j === i ? e.target.value : a)))
                }
                autoFocus={i === 0}
              />
            </label>
          ))}
        </div>
        <div className="mt-3 flex justify-end gap-2">
          <button
            className="rounded px-3 py-1 text-neutral-400 hover:bg-neutral-800"
            onClick={() => submit(null)}
          >
            取消
          </button>
          <button
            className="rounded bg-blue-600 px-3 py-1 text-white hover:bg-blue-500"
            onClick={() => submit(answers)}
          >
            确定
          </button>
        </div>
      </div>
    </div>
  );
}
