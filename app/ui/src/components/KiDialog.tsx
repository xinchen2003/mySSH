import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAppStore } from '../state/app-store';
import type { KiChallengeFrame } from '../term/types';
import { Dialog } from './Dialog';

/**
 * keyboard-interactive（2FA）逐轮应答弹窗。
 * echo=false 的提问用密码输入框（不回显）。
 *
 * 批次四 10.5 特殊规则：
 * - Esc = 取消（submit(null)，即拒绝/中止认证），绝不视为认证成功；
 * - 初始焦点在第一个输入框（基座默认行为）；多 pane 排队语义（pendingKis 队列）不变。
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
    <Dialog
      title="键盘交互认证"
      onClose={() => submit(null)}
      closeOnBackdrop={false}
      backdropClass="z-20"
      panelClass="w-96 rounded-lg border border-neutral-700 bg-neutral-900 p-4 text-sm text-neutral-200 shadow-xl"
    >
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
    </Dialog>
  );
}
