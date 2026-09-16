import { describe, expect, it } from 'vitest';
import { bytesToB64, stripTrailingZmodemHeader } from './file-transfer';

describe('bytesToB64', () => {
  it('空数组 → 空串', () => {
    expect(bytesToB64(new Uint8Array(0))).toBe('');
  });

  it('RFC 4648 测试向量', () => {
    expect(bytesToB64(new Uint8Array([102]))).toBe('Zg==');
    expect(bytesToB64(new Uint8Array([102, 111]))).toBe('Zm8=');
    expect(bytesToB64(new Uint8Array([102, 111, 111]))).toBe('Zm9v');
    expect(bytesToB64(new Uint8Array([102, 111, 111, 98]))).toBe('Zm9vYg==');
  });

  it('全字节值域（0..255）往返', () => {
    const bytes = new Uint8Array(256);
    for (let i = 0; i < 256; i++) bytes[i] = i;
    const b64 = bytesToB64(bytes);
    // atob 解回校验（浏览器环境）
    const bin = atob(b64);
    expect(bin.length).toBe(256);
    for (let i = 0; i < 256; i++) expect(bin.charCodeAt(i)).toBe(i);
  });

  it('跨分块边界（step 0x8000）长度正确', () => {
    const bytes = new Uint8Array(0x8000 + 7).fill(65);
    const b64 = bytesToB64(bytes);
    expect(b64.length).toBe(Math.ceil((0x8000 + 7) / 3) * 4);
    expect(atob(b64).length).toBe(0x8000 + 7);
  });
});

describe('stripTrailingZmodemHeader', () => {
  const zsHeader = (hexTail = '00000000000000') => {
    // **<CAN>B + 14 hex + CR + 0x8A + XON（lrzsz ZRQINIT/ZRINIT 完整尾序）
    const hex = Array.from(hexTail).map((c) => c.charCodeAt(0));
    return new Uint8Array([0x2a, 0x2a, 0x18, 0x42, ...hex, 0x0d, 0x8a, 0x11]);
  };

  it('尾部帧头被剥掉，前导文本保留', () => {
    const text = new TextEncoder().encode('sz file.bin\r\n');
    const merged = new Uint8Array(text.length + 21);
    merged.set(text);
    merged.set(zsHeader(), text.length);
    const out = stripTrailingZmodemHeader([merged]);
    expect(out.length).toBe(1);
    expect(new TextDecoder().decode(out[0])).toBe('sz file.bin\r\n');
  });

  it('整帧只有帧头 → 空（全部静默）', () => {
    expect(stripTrailingZmodemHeader([zsHeader()])).toEqual([]);
  });

  it('跨 piece 的帧头也能剥', () => {
    const h = zsHeader();
    const out = stripTrailingZmodemHeader([h.subarray(0, 5), h.subarray(5)]);
    expect(out).toEqual([]);
  });

  it('无帧头/帧头不在尾部/长度不足 → 原样返回', () => {
    const plain = new TextEncoder().encode('hello **world\r\n');
    expect(stripTrailingZmodemHeader([plain])[0]).toBe(plain); // 引用相等=未动
    const mid = new Uint8Array([...zsHeader(), 0x41, 0x42]); // 帧头后还有字节
    expect(stripTrailingZmodemHeader([mid])[0]).toBe(mid);
    expect(stripTrailingZmodemHeader([new Uint8Array([0x2a, 0x2a, 0x18])])).toHaveLength(1);
  });

  it('hex 区含非 hex 字符不误剥', () => {
    const bad = zsHeader('zzzzzzzzzzzzzz');
    expect(stripTrailingZmodemHeader([bad])[0]).toBe(bad);
  });
});
