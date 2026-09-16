import { describe, expect, it } from 'vitest';
import { bytesToB64 } from './file-transfer';

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
