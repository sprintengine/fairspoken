import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import ts from "typescript";
const source = await readFile(new URL("../src/shortcutRegistration.ts", import.meta.url), "utf8");
const compiled = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ES2020 } }).outputText;
const { replaceShortcuts } = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`);
const old = { recording: "Ctrl+1", stack: "Ctrl+2" };
const next = { recording: "Ctrl+3", stack: "Ctrl+4" };
function fixture(failRegister = "", failRemove = "") {
  const registered = new Set(Object.values(old));
  const io = {
    async register(_role, key) { if (key === failRegister || registered.has(key)) throw new Error("Unavailable"); registered.add(key); },
    async unregister(key) { if (key === failRemove) throw new Error("Removal failed"); registered.delete(key); },
  };
  return { registered, io };
}
{
  const { registered, io } = fixture();
  let saved = false;
  await replaceShortcuts(old, next, io, async () => { saved = true; });
  assert(saved);
  assert.deepEqual(registered, new Set(Object.values(next)));
}
for (const [failRegister, failRemove, failSave] of [["Ctrl+3", "", false], ["Ctrl+4", "", false], ["", "Ctrl+2", false], ["", "", true]]) {
  const { registered, io } = fixture(failRegister, failRemove);
  let saved = false;
  await assert.rejects(replaceShortcuts(old, next, io, async () => {
    if (failSave) throw new Error("Disk unavailable");
    saved = true;
  }));
  assert(!saved);
  assert.deepEqual(registered, new Set(Object.values(old)), "failed replacements retain both previous chords");
}
{
  const { registered, io } = fixture();
  await replaceShortcuts(old, { ...old, stack: "Ctrl+4" }, io, async () => {});
  assert.deepEqual(registered, new Set(["Ctrl+1", "Ctrl+4"]));
}
console.log("Shortcut transaction: 6 checks passed (success, first/second conflict, removal failure, persistence failure, single replacement).");
