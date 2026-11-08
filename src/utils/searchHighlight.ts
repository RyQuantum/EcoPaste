export interface HighlightRange {
  start: number;
  end: number;
}

/**
 * fork：搜索框按空白拆成多个词、每个词各自匹配（与后端 `KeywordFilter` 一致），
 * 这里同样逐词找出 `text` 里所有命中的位置，重叠或相邻的区间合并成一段，按出现顺序返回。
 * 大小写不敏感，按 UTF-16 码元切分，与原单词高亮的规则相同。
 */
export function findHighlightRanges(
  text: string,
  keyword?: string,
): HighlightRange[] {
  const tokens = keyword?.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!tokens?.length) return [];

  const lowerText = text.toLowerCase();
  const ranges: HighlightRange[] = [];

  for (const token of new Set(tokens)) {
    let index = lowerText.indexOf(token);

    while (index !== -1) {
      ranges.push({ end: index + token.length, start: index });
      index = lowerText.indexOf(token, index + token.length);
    }
  }

  ranges.sort((a, b) => {
    return a.start - b.start;
  });

  const merged: HighlightRange[] = [];

  for (const range of ranges) {
    const last = merged[merged.length - 1];

    if (last && range.start <= last.end) {
      last.end = Math.max(last.end, range.end);
      continue;
    }

    merged.push({ ...range });
  }

  return merged;
}
