/**
 * 子序列模糊匹配（命令面板/侧栏搜索共用）。
 * 返回分数（越高越好）或 null（不匹配）。打分偏好：连续命中 > 词首 > 散布。
 */

export function fuzzyScore(query: string, text: string): number | null {
  const q = query.toLowerCase();
  const t = text.toLowerCase();
  if (q.length === 0) return 0;
  let score = 0;
  let ti = 0;
  let streak = 0;
  for (const ch of q) {
    const idx = t.indexOf(ch, ti);
    if (idx === -1) return null;
    const wordStart = idx === 0 || /[\s/\\@:._-]/.test(t[idx - 1]);
    if (idx === ti) {
      streak += 1;
      score += 2 + streak; // 连续命中递增
    } else {
      streak = 0;
      score += wordStart ? 2 : 1;
    }
    ti = idx + 1;
  }
  // 短串优先（同分时更精确的排前）
  return score * 1000 - t.length;
}

/** 对多字段取最佳分 */
export function fuzzyMatchAny(query: string, fields: (string | null | undefined)[]): number | null {
  let best: number | null = null;
  for (const f of fields) {
    if (!f) continue;
    const s = fuzzyScore(query, f);
    if (s !== null && (best === null || s > best)) best = s;
  }
  return best;
}
