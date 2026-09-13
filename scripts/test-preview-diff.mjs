import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
const source = await readFile(new URL('../src/previewDiff.ts', import.meta.url), 'utf8');
const output = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2020, module: ts.ModuleKind.ES2020 } }).outputText;
const { diffPreview } = await import(`data:text/javascript;base64,${Buffer.from(output).toString('base64')}`);
function check(before, after) {
  const runs = diffPreview(before, after);
  assert.equal(runs.filter(r => r.kind !== 'insert').map(r => r.text).join(''), before);
  assert.equal(runs.filter(r => r.kind !== 'delete').map(r => r.text).join(''), after);
  return runs;
}
for (const pair of [
  ['', ''], ['', 'Added words'], ['um ', ''], ['unchanged', 'unchanged'],
  ['hello world', 'Hello, world.'], ['send um the draft', 'send the draft'],
  [' hi\n\tthere ', 'Hi\nthere.'], ['cafe\u0301 🌍 你好', 'Café 🌎 你好！'],
  ['<script>alert("a")</script>', '<script>alert("b")</script>'],
]) check(...pair);
const changes = check('hello sam keep these words and send teh draft', 'Hello Sam, keep these words and send the draft.');
assert(changes.some(r => r.kind === 'equal' && r.text.includes('keep these words and send')));
assert(changes.filter(r => r.kind === 'insert').length > 1);
const old = Array.from({ length: 4000 }, (_, i) => 'old' + i).join(' ');
const next = Array.from({ length: 4000 }, (_, i) => 'new' + i).join(' ');
assert.equal(check(old, next).length, 2, 'large rewrite uses bounded exact replacement');
let seed = 47;
const random = () => { seed = (seed * 1664525 + 1013904223) >>> 0; return seed; };
const alphabet = ['word', ' ', '\n', ',', '🌍', 'é', '好'];
for (let test = 0; test < 100; test++) {
  const make = () => Array.from({ length: random() % 30 }, () => alphabet[random() % alphabet.length]).join('');
  check(make(), make());
}
console.log('Preview diff: exact reconstruction, independent edits, Unicode, whitespace, literal markup, 100 generated pairs and bounded large rewrite passed.');
