import { test } from "node:test";
import assert from "node:assert/strict";
import { phoneticKey } from "./phonetic-key.mjs";

// Every expected key below is what `normalize` in src-tauri/src/phonetic_index.rs
// returns for the same input. Change them only together with the Rust.

test("symbols that are part of a name are spelled out, others separate words", () => {
  assert.equal(phoneticKey("C++"), "cplusplus");
  assert.equal(phoneticKey("c+++"), "cplusplusplus");
  assert.equal(phoneticKey("C#"), "csharp");
  assert.equal(phoneticKey("F#"), "fsharp");
  // A `+` that does not close a word is a separator.
  assert.equal(phoneticKey("c++x"), "cx");
  assert.equal(phoneticKey("a+b"), "ab");
  assert.equal(phoneticKey("#foo"), "foo");
});

test("a dot opening a word is spelled out, a dot inside one is not", () => {
  assert.equal(phoneticKey(".NET"), "dotnet");
  assert.equal(phoneticKey("(.net)"), "dotnet");
  assert.equal(phoneticKey("ASP.NET Core"), "aspnetcore");
  assert.equal(phoneticKey("x.NET"), "xnet");
  assert.equal(phoneticKey("Next.js"), "nextjs");
  assert.equal(phoneticKey("CI/CD"), "cicd");
});

test("only the app's table of Latin diacritics is folded; other letters are kept", () => {
  assert.equal(phoneticKey("Dún Laoghaire"), "dunlaoghaire");
  assert.equal(phoneticKey("naïve"), "naive");
  assert.equal(phoneticKey("Łódź"), "lodz");
  assert.equal(phoneticKey("Straße"), "straße");
  assert.equal(phoneticKey("Őrség"), "őrseg");
  assert.equal(phoneticKey("İstanbul"), "i̇stanbul");
  assert.equal(phoneticKey("日本"), "日本");
  assert.equal(phoneticKey("Ⅻ"), "ⅻ");
  assert.equal(phoneticKey("HbA1c"), "hba1c");
  assert.equal(phoneticKey("co-codamol"), "cocodamol");
});
