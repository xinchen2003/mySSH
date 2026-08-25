import { describe, expect, it } from 'vitest';
import { KEY_ACTIONS, effectiveBindings, isValidCombo, matchAction, matchCombo } from './keymap';

/** node 环境无 KeyboardEvent 构造器；matchCombo 只读这几个字段 */
const ev = (
  key: string,
  code: string,
  mods: { ctrl?: boolean; alt?: boolean; shift?: boolean } = {},
): KeyboardEvent =>
  ({
    key,
    code,
    ctrlKey: mods.ctrl ?? false,
    altKey: mods.alt ?? false,
    shiftKey: mods.shift ?? false,
  }) as KeyboardEvent;

describe('matchCombo', () => {
  it('字符键大小写不敏感', () => {
    expect(matchCombo(ev('P', 'KeyP', { ctrl: true, shift: true }), 'Ctrl+Shift+P')).toBe(true);
    expect(matchCombo(ev('p', 'KeyP', { ctrl: true, shift: true }), 'Ctrl+Shift+P')).toBe(true);
  });

  it('修饰键必须精确相等：Ctrl+C 不命中 Ctrl+Shift+C', () => {
    expect(matchCombo(ev('c', 'KeyC', { ctrl: true }), 'Ctrl+Shift+C')).toBe(false);
    expect(matchCombo(ev('c', 'KeyC', { ctrl: true, shift: true }), 'Ctrl+Shift+C')).toBe(true);
  });

  it('特殊键按 code 语义名匹配（Insert/Delete/方向键）', () => {
    expect(matchCombo(ev('Insert', 'Insert', { ctrl: true }), 'Ctrl+Insert')).toBe(true);
    expect(matchCombo(ev('Insert', 'Insert', { shift: true }), 'Shift+Insert')).toBe(true);
    expect(matchCombo(ev('Insert', 'Insert', {}), 'Shift+Insert')).toBe(false);
  });
});

describe('matchAction（主绑定 + 固定别名）', () => {
  it('别名 Ctrl+Insert / Shift+Insert 命中复制粘贴', () => {
    const b = effectiveBindings('default');
    expect(matchAction(ev('Insert', 'Insert', { ctrl: true }), b, 'copy')).toBe(true);
    expect(matchAction(ev('Insert', 'Insert', { shift: true }), b, 'paste')).toBe(true);
  });

  it('自定义覆盖主绑定后别名仍有效', () => {
    const b = effectiveBindings('default', { copy: 'Ctrl+Alt+C' });
    expect(matchAction(ev('c', 'KeyC', { ctrl: true, alt: true }), b, 'copy')).toBe(true);
    expect(matchAction(ev('Insert', 'Insert', { ctrl: true }), b, 'copy')).toBe(true);
    // 原默认绑定已被覆盖，不再命中
    expect(matchAction(ev('c', 'KeyC', { ctrl: true, shift: true }), b, 'copy')).toBe(false);
  });

  it('无别名的动作只认主绑定', () => {
    const b = effectiveBindings('default');
    expect(matchAction(ev('Insert', 'Insert', { ctrl: true }), b, 'palette')).toBe(false);
  });
});

describe('复制/粘贴动作注册', () => {
  it('copy/paste 在注册表且默认绑定正确', () => {
    const copy = KEY_ACTIONS.find((a) => a.id === 'copy');
    const paste = KEY_ACTIONS.find((a) => a.id === 'paste');
    expect(copy?.default).toBe('Ctrl+Shift+C');
    expect(copy?.alias).toBe('Ctrl+Insert');
    expect(paste?.default).toBe('Ctrl+Shift+V');
    expect(paste?.alias).toBe('Shift+Insert');
  });

  it('别名组合本身是合法组合键', () => {
    for (const a of KEY_ACTIONS) {
      if (a.alias) expect(isValidCombo(a.alias)).toBe(true);
    }
  });
});
