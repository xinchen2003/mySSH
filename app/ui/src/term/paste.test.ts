import { describe, expect, it } from 'vitest';
import { pasteNeedsConfirm } from './paste';

describe('pasteNeedsConfirm 多行粘贴判定', () => {
  it('单行（含/不含尾换行）直贴', () => {
    expect(pasteNeedsConfirm('ls')).toBeNull();
    expect(pasteNeedsConfirm('ls\n')).toBeNull(); // 复制常见的单个尾换行
    expect(pasteNeedsConfirm('ls\r\n')).toBeNull(); // CRLF 尾换行同样剥掉
  });

  it('空文本与单个换行直贴（后者等于按一次回车）', () => {
    expect(pasteNeedsConfirm('')).toBeNull();
    expect(pasteNeedsConfirm('\n')).toBeNull();
  });

  it('多行 → 需确认，行数按剥离尾换行后的实际行计', () => {
    expect(pasteNeedsConfirm('ls\npwd')).toEqual({ lines: 2 });
    expect(pasteNeedsConfirm('ls\npwd\n')).toEqual({ lines: 2 });
    expect(pasteNeedsConfirm('a\r\nb\r\n')).toEqual({ lines: 2 });
  });

  it('连续空行也算一行（两个尾换行只剥一个）', () => {
    expect(pasteNeedsConfirm('a\n\n')).toEqual({ lines: 2 });
    expect(pasteNeedsConfirm('a\n\n\nb')).toEqual({ lines: 4 });
  });
});
