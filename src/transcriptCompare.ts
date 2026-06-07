// One shared word-level comparison primitive for every surface that has to show
// "what did the transcriber actually hear?" — the voice accuracy test, the speed
// test's speaking leg, and their live previews. It replaces the order-blind
// multiset match that used to live in speedTest.ts.
//
// A multiset count hides word order, can't tell a substitution from a dropped
// word, and can read as correct when a word repeats. This does a real sequence
// alignment (word-level Levenshtein with a deterministic backtrace), so every
// target word is a hit, a miss, or a substitution, and every transcript word is
// a hit, an insertion, or a substitution. It is deliberately DOM-free and pure
// so it can be exercised independently of Tauri.

export type TokenState = "hit" | "miss" | "inserted" | "substituted" | "pending";

export interface ComparedToken {
  /** The token exactly as it appeared, for display (keeps caps + punctuation). */
  raw: string;
  /** Lowercased, punctuation-stripped form used for matching. */
  normalized: string;
  state: TokenState;
  /** Index of the aligned token on the other side, or null when unmatched. */
  counterpartIndex: number | null;
}

export interface TranscriptComparison {
  /** Expected words, in order, each tagged hit / miss / substituted. */
  targetTokens: ComparedToken[];
  /** Heard words, in order, each tagged hit / inserted / substituted. */
  transcriptTokens: ComparedToken[];
  hits: number;
  /** Target words that didn't come through correctly (deletions + substitutions). */
  misses: number;
  /** Extra transcript words with no target counterpart. */
  insertions: number;
  /** Target words aligned to a different transcript word. */
  substitutions: number;
  /** Target words with no transcript counterpart at all. */
  deletions: number;
  /** Target word count — the denominator for accuracy. */
  total: number;
  /** Rounded percent of target words heard correctly (hits / total). */
  accuracy: number;
}

/** Lowercase and drop everything that isn't a letter or number, so the
 *  transcriber's capitalization and punctuation can't tank a real match. */
export function normalizeToken(raw: string): string {
  return raw.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, "");
}

interface RawToken {
  raw: string;
  normalized: string;
}

/** Split on whitespace, keep the raw token for display, and drop tokens that
 *  normalize to nothing (stray punctuation) so they don't skew the counts. */
function tokenize(text: string): RawToken[] {
  return text
    .trim()
    .split(/\s+/)
    .filter(Boolean)
    .map((raw) => ({ raw, normalized: normalizeToken(raw) }))
    .filter((token) => token.normalized.length > 0);
}

type Op = "match" | "sub" | "del" | "ins";
interface AlignStep {
  op: Op;
  /** Target index, or -1 for an insertion. */
  ti: number;
  /** Transcript index, or -1 for a deletion. */
  ri: number;
}

/** Compare a transcript against the expected passage with a word-level sequence
 *  alignment. `target` is the expected passage; `transcript` is what the backend
 *  returned. The result carries display-ready token models for both sides plus
 *  the hit/miss/insertion/substitution/deletion counts and an accuracy percent. */
export function compareTranscript(transcript: string, target: string): TranscriptComparison {
  const targetRaw = tokenize(target);
  const transRaw = tokenize(transcript);
  const n = targetRaw.length;
  const m = transRaw.length;

  // Edit-distance grid: d[i][j] = cost to turn target[0..i) into transcript[0..j).
  const d: number[][] = Array.from({ length: n + 1 }, () => new Array<number>(m + 1).fill(0));
  for (let i = 0; i <= n; i++) d[i][0] = i; // delete every remaining target word
  for (let j = 0; j <= m; j++) d[0][j] = j; // insert every remaining transcript word
  for (let i = 1; i <= n; i++) {
    for (let j = 1; j <= m; j++) {
      const subCost = targetRaw[i - 1].normalized === transRaw[j - 1].normalized ? 0 : 1;
      d[i][j] = Math.min(
        d[i - 1][j - 1] + subCost, // match or substitute
        d[i - 1][j] + 1, // deletion (target word unheard)
        d[i][j - 1] + 1, // insertion (extra heard word)
      );
    }
  }

  // Backtrace with a fixed precedence — diagonal, then deletion, then insertion —
  // so repeated words always align the same way for the same inputs.
  const steps: AlignStep[] = [];
  let i = n;
  let j = m;
  while (i > 0 || j > 0) {
    if (i > 0 && j > 0) {
      const subCost = targetRaw[i - 1].normalized === transRaw[j - 1].normalized ? 0 : 1;
      if (d[i][j] === d[i - 1][j - 1] + subCost) {
        steps.push({ op: subCost === 0 ? "match" : "sub", ti: i - 1, ri: j - 1 });
        i--;
        j--;
        continue;
      }
    }
    if (i > 0 && d[i][j] === d[i - 1][j] + 1) {
      steps.push({ op: "del", ti: i - 1, ri: -1 });
      i--;
      continue;
    }
    steps.push({ op: "ins", ti: -1, ri: j - 1 });
    j--;
  }

  const targetTokens: ComparedToken[] = targetRaw.map((token) => ({
    raw: token.raw,
    normalized: token.normalized,
    state: "miss",
    counterpartIndex: null,
  }));
  const transcriptTokens: ComparedToken[] = transRaw.map((token) => ({
    raw: token.raw,
    normalized: token.normalized,
    state: "inserted",
    counterpartIndex: null,
  }));

  let hits = 0;
  let substitutions = 0;
  let deletions = 0;
  let insertions = 0;
  for (const step of steps) {
    switch (step.op) {
      case "match":
        targetTokens[step.ti].state = "hit";
        targetTokens[step.ti].counterpartIndex = step.ri;
        transcriptTokens[step.ri].state = "hit";
        transcriptTokens[step.ri].counterpartIndex = step.ti;
        hits++;
        break;
      case "sub":
        targetTokens[step.ti].state = "substituted";
        targetTokens[step.ti].counterpartIndex = step.ri;
        transcriptTokens[step.ri].state = "substituted";
        transcriptTokens[step.ri].counterpartIndex = step.ti;
        substitutions++;
        break;
      case "del":
        targetTokens[step.ti].state = "miss";
        deletions++;
        break;
      case "ins":
        transcriptTokens[step.ri].state = "inserted";
        insertions++;
        break;
    }
  }

  const total = n;
  const misses = deletions + substitutions;
  const accuracy = total > 0 ? Math.round((hits / total) * 100) : m === 0 ? 100 : 0;

  return {
    targetTokens,
    transcriptTokens,
    hits,
    misses,
    insertions,
    substitutions,
    deletions,
    total,
    accuracy,
  };
}
