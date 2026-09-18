import { createTrzszTriggerGuard } from './file-transfer';
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

describe('createTrzszTriggerGuard 触发串跨块保险', () => {
  const TE = new TextEncoder();
  const TD = new TextDecoder('latin1');
  const TRIG = '\x1b7\x07::TRZSZ:TRANSFER:S:1.2.0:8972115042400:43958\r\n';

  function recorder() {
    const out: string[] = [];
    const guard = createTrzszTriggerGuard((c) => out.push(TD.decode(c)));
    return { out, guard };
  }
  const feed = (guard: { push(c: Uint8Array): void }, s: string) => guard.push(TE.encode(s));

  it('完整触发行单块到达 → 立即整块转发', () => {
    const { out, guard } = recorder();
    feed(guard, TRIG);
    expect(out).toEqual([TRIG]);
  });

  it('触发行在任意点被切块 → 暂存拼回，完整到达才转发', () => {
    // 遍历所有切点：任何切法都必须最终产出完整且连续的触发行
    for (let cut = 1; cut < TRIG.length; cut++) {
      const { out, guard } = recorder();
      feed(guard, TRIG.slice(0, cut));
      // 除「已完成的前缀文本」外，触发残片不得提前转发
      const early = out.join('');
      expect(early.includes('::TRZSZ:TRANSFER')).toBe(false);
      feed(guard, TRIG.slice(cut));
      expect(out.join('')).toBe(TRIG);
    }
  });

  it('触发行前有正文（前一行）：正文先走，触发残片暂存', () => {
    // 触发行恒在行首（tsz 在命令回显的换行后输出）；守卫只扫行尾片段
    const { out, guard } = recorder();
    feed(guard, 'root@host ~# tsz f.bin\r\n' + TRIG.slice(0, 20));
    expect(out).toEqual(['root@host ~# tsz f.bin\r\n']);
    feed(guard, TRIG.slice(20));
    expect(out.join('')).toBe('root@host ~# tsz f.bin\r\n' + TRIG);
  });

  it('普通无换行行尾（shell 提示符）立即放行，不留滞', () => {
    const { out, guard } = recorder();
    feed(guard, 'root@host ~# ');
    expect(out).toEqual(['root@host ~# ']);
  });

  it('非触发行尾不暂存', () => {
    const { out, guard } = recorder();
    feed(guard, 'abc\nxyz');
    expect(out).toEqual(['abc\nxyz']);
  });

  it('flush 放行暂存残片（会话关闭兜底）', () => {
    const { out, guard } = recorder();
    feed(guard, TRIG.slice(0, 15));
    expect(out).toEqual([]);
    guard.flush();
    expect(out).toEqual([TRIG.slice(0, 15)]);
  });

  it('超过暂存上限的行尾直接放行', () => {
    const { out, guard } = recorder();
    const long = TRIG.slice(0, 10) + 'x'.repeat(200);
    feed(guard, long);
    expect(out).toEqual([long]);
  });
});
