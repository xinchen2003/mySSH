import { describe, expect, it } from 'vitest';

import { fuzzyMatchAny, fuzzyScore } from './fuzzy';

describe('fuzzyScore', () => {
  it('子序列命中', () => {
    expect(fuzzyScore('web', 'prod-web-01')).not.toBeNull();
    expect(fuzzyScore('pw1', 'prod-web-01')).not.toBeNull();
  });

  it('非子序列不匹配', () => {
    expect(fuzzyScore('xyz', 'prod-web-01')).toBeNull();
  });

  it('连续前缀优于散布命中', () => {
    const a = fuzzyScore('prod', 'prod-web') ?? -1;
    const b = fuzzyScore('prod', 'p-r-o-d-server') ?? -1;
    expect(a).toBeGreaterThan(b);
  });

  it('大小写不敏感 + 多字段取优', () => {
    expect(fuzzyScore('ROOT', 'root@10.0.0.1')).not.toBeNull();
    expect(fuzzyMatchAny('db', ['生产', null, 'mysql-db'])).not.toBeNull();
    expect(fuzzyMatchAny('db', ['生产', 'web'])).toBeNull();
  });
});
