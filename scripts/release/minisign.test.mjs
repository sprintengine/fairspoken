// node --test scripts/release/minisign.test.mjs  (npm run test:release)

import assert from 'node:assert/strict'
import { generateKeyPairSync, sign } from 'node:crypto'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

import { ed25519Problem, minisignProblem, parseMinisignPublicKey } from './minisign.mjs'

// Real output of `tauri signer generate` / `tauri signer sign` (Tauri CLI
// 2.11) for a throwaway key made only for this test, over the six bytes
// "hello\n". Not the release key.
const PUBLIC_KEY =
  'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEFBOUFCNDVGNzk1MEVFRjgKUldUNDdsQjVYN1NhcWhlUTdFenRSeHV4dkFMcTArdkhHbENpdEQvM2J2MzNudUN2TjJCdUF2bGgK'
const OTHER_PUBLIC_KEY =
  'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDc3RTg5NzQwRjYwODUzMjUKUldRbFV3ajJRSmZvZCtSYW9rbytzVWh4bmpQbFdaSFBLZXdpQzkvcjF6ZnlQTGFtZUZ3N1BwbnYK'
const SIGNATURE =
  'dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVUNDdsQjVYN1NhcXNqSjE1bUFDZ3lyWDNUT1lMcFZsNVBsUmlNcElHSU1GNEwvOC9lVnRnMmI2N1JDWTFad3RzQnFabEFtbE5lOWc3K3BsNnhKVEpPSmU2OWpqNm9COGdVPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkxMjAyNjkyCWZpbGU6Zi50YXIuZ3oKNzVkV1VKbXN5ZnEyWnkyWEdPaittaURpZ01lQVJxR01lWWh0SUhaOW9LRm1HZ05jTDRqVzFMVXE3dWhYZnhNTk9aZmZydXBoY3Y2bmJXbVNaU2JtRGc9PQo='
const DATA = Buffer.from('hello\n')

test('a tauri signer signature verifies against its public key', () => {
  assert.equal(minisignProblem({ data: DATA, signature: SIGNATURE, publicKey: PUBLIC_KEY }), null)
  // With the trailing newline the .sig file has, as read from disk.
  assert.equal(minisignProblem({ data: DATA, signature: `${SIGNATURE}\n`, publicKey: PUBLIC_KEY }), null)
})

test('the wrong file, the wrong key or a mangled signature are each named', () => {
  assert.equal(minisignProblem({ data: Buffer.from('hello'), signature: SIGNATURE, publicKey: PUBLIC_KEY }), 'the signature does not match the file')
  assert.match(minisignProblem({ data: DATA, signature: SIGNATURE, publicKey: OTHER_PUBLIC_KEY }), /^signed by key AA9AB45F7950EEF8, not the shipped key 77E89740F6085325/)
  assert.match(minisignProblem({ data: DATA, signature: 'bm90IGEgc2lnbmF0dXJl', publicKey: PUBLIC_KEY }), /not a minisign signature file/)
  assert.match(minisignProblem({ data: DATA, signature: '', publicKey: PUBLIC_KEY }), /signature is empty/)
  assert.throws(() => parseMinisignPublicKey('aGVsbG8='), /Not a minisign Ed25519 public key/)
  // A forged trusted comment (another file name) breaks the global signature.
  const lines = Buffer.from(SIGNATURE, 'base64').toString('utf8').split('\n')
  lines[2] = lines[2].replace('file:f.tar.gz', 'file:other.tar.gz')
  const forged = Buffer.from(lines.join('\n')).toString('base64')
  assert.equal(minisignProblem({ data: DATA, signature: forged, publicKey: PUBLIC_KEY }), 'the trusted comment signature does not match')
})

test("the shipped updater public key in tauri.conf.json parses as a minisign key", () => {
  const config = JSON.parse(readFileSync(new URL('../../src-tauri/tauri.conf.json', import.meta.url), 'utf8'))
  assert.equal(parseMinisignPublicKey(config.plugins.updater.pubkey).keyId.length, 8)
})

test('Sparkle EdDSA: a bare Ed25519 signature checks against SUPublicEDKey', () => {
  const { publicKey, privateKey } = generateKeyPairSync('ed25519')
  const raw = publicKey.export({ format: 'der', type: 'spki' }).subarray(12).toString('base64')
  const zip = Buffer.from('PK\u0003\u0004 a zip, more or less')
  const signature = sign(null, zip, privateKey).toString('base64')
  assert.equal(ed25519Problem({ data: zip, signature, publicKey: raw }), null)
  assert.equal(ed25519Problem({ data: Buffer.from('other'), signature, publicKey: raw }), 'the EdDSA signature does not match the archive')
  // The contract key, with a signature made by some other key.
  assert.equal(
    ed25519Problem({ data: zip, signature, publicKey: 'SPGwzOHQk1H5lvqePdjg11xTC8LT3/AX3kILlbOIxp0=' }),
    'the EdDSA signature does not match the archive',
  )
  assert.match(ed25519Problem({ data: zip, signature, publicKey: '' }), /not 32 bytes/)
  assert.match(ed25519Problem({ data: zip, signature: 'abc', publicKey: raw }), /not 64 bytes/)
})
