import { useState } from 'react';
import type { ITheme } from '@xterm/xterm';
import { useAppStore } from '../state/app-store';
import { BUILTIN_THEMES } from '../term/themes';
import { useT } from '../i18n';

const inputCls =
  'rounded border border-neutral-700 bg-neutral-800 px-2 py-1 text-xs text-neutral-200 outline-none focus:border-blue-500 focus-visible:ring-1 focus-visible:ring-neutral-500';

/** ANSI 16 色语义名（ITheme 字段顺序：8 常规 + 8 亮档） */
const ANSI_KEYS = [
  'black',
  'red',
  'green',
  'yellow',
  'blue',
  'magenta',
  'cyan',
  'white',
  'brightBlack',
  'brightRed',
  'brightGreen',
  'brightYellow',
  'brightBlue',
  'brightMagenta',
  'brightCyan',
  'brightWhite',
] as const;

type AnsiKey = (typeof ANSI_KEYS)[number];
type BasicKey = 'background' | 'foreground' | 'cursor' | 'selectionBackground';

/** 草稿 = xterm ITheme + ui 明暗档（存储格式与 resolveTheme 的 custom 分支一致） */
interface Draft extends ITheme {
  ui?: 'dark' | 'light';
}

/** 兜底调色板：草稿缺色/坏 JSON 时按 one-dark 显示 */
const SEED: Draft = { ui: 'dark', ...BUILTIN_THEMES[0].xterm };

function parseDraft(json: string): Draft {
  try {
    return JSON.parse(json) as Draft;
  } catch {
    return SEED; // 坏 JSON：以 one-dark 为底
  }
}

/** 取色：草稿缺失或非法（input[type=color] 只收 #rrggbb）时回退 SEED */
function pick(draft: Draft, key: BasicKey | AnsiKey): string {
  const v = draft[key];
  return typeof v === 'string' && /^#[0-9a-fA-F]{6}$/.test(v) ? v : (SEED[key] as string);
}

/**
 * 自定义主题图形化编辑器（theme === 'custom' 时替代原 JSON textarea）。
 * 单一事实源在 settings KV：每次修改即整体 stringify 写回 theme.customJson；
 * 坏 JSON 下首次修改会把 one-dark 种子全量铺入，产出恒为合法 ITheme JSON。
 */
export function ThemeStudio() {
  const settings = useAppStore((s) => s.settings);
  const setSetting = useAppStore((s) => s.setSetting);
  const t = useT();
  const customJson =
    typeof settings['theme.customJson'] === 'string' ? settings['theme.customJson'] : '';
  const draft = parseDraft(customJson);
  const uiMode = draft.ui === 'light' ? 'light' : 'dark';
  const [baseId, setBaseId] = useState(BUILTIN_THEMES[0].id);

  /** 局部改色：并入当前草稿（坏 JSON 时已回退种子）后整体写回 */
  const patch = (p: Partial<Draft>) =>
    setSetting('theme.customJson', JSON.stringify({ ...draft, ...p }, null, 2));

  /** 把内置原型全部颜色铺进草稿 */
  const applyBase = () => {
    const base = BUILTIN_THEMES.find((b) => b.id === baseId) ?? BUILTIN_THEMES[0];
    setSetting('theme.customJson', JSON.stringify({ ui: base.ui, ...base.xterm }, null, 2));
  };

  const basics: [BasicKey, string][] = [
    ['background', t('dialogs.studioColorBackground')],
    ['foreground', t('dialogs.studioColorForeground')],
    ['cursor', t('dialogs.studioColorCursor')],
    ['selectionBackground', t('dialogs.studioColorSelection')],
  ];

  return (
    <div
      className="mt-2 rounded border border-neutral-800 p-3"
      aria-label={t('dialogs.themeCustomAria')}
    >
      {/* 基于内置主题 */}
      <div className="mb-3 flex items-center gap-2">
        <span className="text-neutral-500">{t('dialogs.studioBaseTheme')}</span>
        <select className={inputCls} value={baseId} onChange={(e) => setBaseId(e.target.value)}>
          {BUILTIN_THEMES.map((b) => (
            <option key={b.id} value={b.id}>
              {b.label}
            </option>
          ))}
        </select>
        <button
          className="rounded bg-neutral-800 px-2 py-1 text-neutral-300 hover:bg-neutral-700"
          onClick={applyBase}
        >
          {t('dialogs.studioApply')}
        </button>
        <span className="ml-auto flex items-center gap-2 text-neutral-500">
          {t('dialogs.studioUiMode')}
          <label className="flex items-center gap-1">
            <input
              type="radio"
              name="studio-ui-mode"
              checked={uiMode === 'dark'}
              onChange={() => patch({ ui: 'dark' })}
            />
            {t('dialogs.themeDarkBadge')}
          </label>
          <label className="flex items-center gap-1">
            <input
              type="radio"
              name="studio-ui-mode"
              checked={uiMode === 'light'}
              onChange={() => patch({ ui: 'light' })}
            />
            {t('dialogs.themeLightBadge')}
          </label>
        </span>
      </div>

      <div className="flex gap-4">
        <div className="flex-1">
          {/* 基础色 4 项 */}
          <h4 className="mb-1.5 font-semibold text-neutral-300">
            {t('dialogs.studioBasicColors')}
          </h4>
          <div className="mb-3 grid grid-cols-2 gap-x-4 gap-y-1.5">
            {basics.map(([key, label]) => (
              <label key={key} className="flex items-center gap-2">
                <input
                  type="color"
                  className="h-6 w-8 cursor-pointer rounded border border-neutral-700 bg-transparent p-0"
                  value={pick(draft, key)}
                  onChange={(e) => patch({ [key]: e.target.value })}
                />
                <span className="text-neutral-400">{label}</span>
                <span className="ml-auto font-mono text-neutral-500">{pick(draft, key)}</span>
              </label>
            ))}
          </div>

          {/* ANSI 16 色：两行 8 列 */}
          <h4 className="mb-1.5 font-semibold text-neutral-300">{t('dialogs.studioAnsiColors')}</h4>
          <div className="grid grid-cols-8 gap-1.5">
            {ANSI_KEYS.map((key) => (
              <input
                key={key}
                type="color"
                title={key}
                className="h-7 w-full cursor-pointer rounded border border-neutral-700 bg-transparent p-0"
                value={pick(draft, key)}
                onChange={(e) => patch({ [key]: e.target.value })}
              />
            ))}
          </div>
        </div>

        {/* 实时预览：迷你终端 mock */}
        <div
          className="w-52 shrink-0 self-start rounded border border-neutral-700 p-2 font-mono text-[10px] leading-4"
          style={{ background: pick(draft, 'background'), color: pick(draft, 'foreground') }}
        >
          <div>
            <span style={{ color: pick(draft, 'green') }}>$</span> ls
          </div>
          <div>
            <span style={{ color: pick(draft, 'blue') }}>src</span>{' '}
            <span style={{ color: pick(draft, 'cyan') }}>dist</span>{' '}
            <span style={{ color: pick(draft, 'magenta') }}>logs</span>
          </div>
          <div>
            <span style={{ color: pick(draft, 'red') }}>error.log</span>{' '}
            <span style={{ color: pick(draft, 'yellow') }}>README.md</span>
          </div>
          <div>
            <span style={{ background: pick(draft, 'selectionBackground') }}>
              {t('dialogs.studioColorSelection')}
            </span>
          </div>
          <div>
            <span style={{ color: pick(draft, 'green') }}>$</span>{' '}
            <span
              className="inline-block h-3 w-1.5 align-middle"
              style={{ background: pick(draft, 'cursor') }}
            />
          </div>
        </div>
      </div>
    </div>
  );
}
