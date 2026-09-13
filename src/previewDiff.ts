export type PreviewRun = { kind: "equal" | "insert" | "delete"; text: string };

// Keep word spelling changes together, punctuation separate, and every space
// intact. Unicode code points (including surrogate pairs) are never split.
function tokens(text: string): string[] {
  return text.match(/\s+|[\p{L}\p{N}\p{M}_]+|[^\s\p{L}\p{N}\p{M}_]/gu) ?? [];
}

/** Exact reconstruction on either side; capped LCS work keeps live UI responsive. */
export function diffPreview(before: string, after: string): PreviewRun[] {
  if (before === after) return before ? [{ kind: "equal", text: before }] : [];
  const old = tokens(before), next = tokens(after);
  let prefix = 0;
  while (prefix < old.length && prefix < next.length && old[prefix] === next[prefix]) prefix++;
  let oldEnd = old.length, newEnd = next.length;
  while (oldEnd > prefix && newEnd > prefix && old[oldEnd - 1] === next[newEnd - 1]) { oldEnd--; newEnd--; }
  const result: PreviewRun[] = [];
  function add(kind: PreviewRun["kind"], text: string): void {
    if (!text) return;
    const last = result[result.length - 1];
    if (last?.kind === kind) last.text += text;
    else result.push({ kind, text });
  }
  add("equal", old.slice(0, prefix).join(""));
  const rows = oldEnd - prefix, columns = newEnd - prefix, stride = columns + 1;
  // Large rewrites use one exact replacement instead of an unbounded matrix.
  if ((rows + 1) * stride > 250_000) {
    add("delete", old.slice(prefix, oldEnd).join(""));
    add("insert", next.slice(prefix, newEnd).join(""));
  } else {
    const lengths = new Uint32Array((rows + 1) * stride);
    for (let i = rows - 1; i >= 0; i--) {
      for (let j = columns - 1; j >= 0; j--) {
        lengths[i * stride + j] = old[prefix + i] === next[prefix + j]
          ? 1 + lengths[(i + 1) * stride + j + 1]
          : Math.max(lengths[(i + 1) * stride + j], lengths[i * stride + j + 1]);
      }
    }
    let i = 0, j = 0;
    while (i < rows || j < columns) {
      if (i < rows && j < columns && old[prefix + i] === next[prefix + j]) {
        add("equal", old[prefix + i]); i++; j++;
      } else if (i < rows && (j === columns || lengths[(i + 1) * stride + j] >= lengths[i * stride + j + 1])) {
        add("delete", old[prefix + i++]);
      } else add("insert", next[prefix + j++]);
    }
  }
  add("equal", old.slice(oldEnd).join(""));
  return result;
}
