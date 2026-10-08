// The app's term normalisation, ported from `normalize` / `tokenize` in
// src-tauri/src/phonetic_index.rs. The Rust side is the source of truth: the
// app merges pack terms whose keys match (vocabulary_packs.rs), so the pack
// builders must dedupe and check always_on by exactly the same key, or a pack
// can ship two terms the app silently folds into one.
//
// What it does, step for step with the Rust:
// * Words are runs of Unicode alphanumerics (Alphabetic or Numeric, like
//   char::is_alphanumeric), lowercased one code point at a time
//   (char::to_lowercase) and then folded through the same short table of
//   Latin diacritics ("Dún" -> "dun"). Characters outside that table are
//   kept as they are; there is no general accent stripping.
// * A `+` or `#` closing a word is spelled "plus" / "sharp" (each one in a
//   run), so "C", "C++" and "C#" stay apart.
// * A `.` opening a word (at the start, or after whitespace, `/` or `(`, and
//   followed by an alphanumeric) is spelled "dot", so ".NET" is not "net".
// * Any other symbol separates words; the key is the words joined.

const ALPHANUMERIC = /^[\p{Alphabetic}\p{N}]$/u;
const WHITESPACE = /^\p{White_Space}$/u;

const FOLD = new Map(
  Object.entries({
    a: "àáâãäåā",
    c: "çćč",
    e: "èéêëēę",
    i: "ìíîïī",
    n: "ñń",
    o: "òóôõöøō",
    u: "ùúûüū",
    y: "ýÿ",
    s: "šś",
    z: "žźż",
    l: "ł",
  }).flatMap(([plain, accented]) => [...accented].map((ch) => [ch, plain])),
);

const isAlphanumeric = (ch) => ch !== undefined && ALPHANUMERIC.test(ch);

export function phoneticKey(text) {
  const chars = Array.from(text);
  let key = "";
  let current = "";
  chars.forEach((ch, index) => {
    const previous = chars[index - 1];
    const next = chars[index + 1];
    if ((ch === "+" || ch === "#") && current) {
      let end = index;
      while (end < chars.length && chars[end] === ch) end += 1;
      if (end === chars.length || !isAlphanumeric(chars[end])) {
        current += ch === "+" ? "plus" : "sharp";
        return;
      }
    }
    const opensWord =
      !current &&
      (previous === undefined || WHITESPACE.test(previous) || previous === "/" || previous === "(") &&
      isAlphanumeric(next);
    if (ch === "." && opensWord) {
      current += "dot";
      return;
    }
    if (isAlphanumeric(ch)) {
      for (const lower of Array.from(ch.toLowerCase())) current += FOLD.get(lower) ?? lower;
    } else if (current) {
      key += current;
      current = "";
    }
  });
  return key + current;
}
