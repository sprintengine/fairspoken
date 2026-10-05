// Verifies a minisign signature the way the Tauri updater (and the
// transcription host's self-updater) will, so a release whose
// TAURI_SIGNING_PRIVATE_KEY does not match the public key the apps ship fails
// in its own package leg instead of in every installed build.
//
// Both halves are the strings Tauri uses: the public key is
// `plugins.updater.pubkey` in tauri.conf.json (base64 of a minisign public key
// file), the signature is the `.sig` file `tauri signer sign` and
// `createUpdaterArtifacts` write (base64 of a minisign signature file).

import { createHash, createPublicKey, verify } from 'node:crypto'

const ED25519_SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex')

function decodeLines(base64, what) {
  const text = Buffer.from(String(base64 ?? '').trim(), 'base64').toString('utf8')
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter(Boolean)
  if (lines.length === 0) throw new Error(`${what} is empty`)
  return lines
}

export function parseMinisignPublicKey(base64) {
  const lines = decodeLines(base64, 'The minisign public key')
  const keyLine = lines.find((line) => !line.startsWith('untrusted comment:'))
  const raw = Buffer.from(keyLine ?? '', 'base64')
  if (raw.length !== 42 || raw.subarray(0, 2).toString('latin1') !== 'Ed') {
    throw new Error('Not a minisign Ed25519 public key')
  }
  return {
    keyId: raw.subarray(2, 10),
    key: createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, raw.subarray(10)]), format: 'der', type: 'spki' }),
  }
}

// Returns the reason the signature does not hold, or null when it does.
export function minisignProblem({ data, signature, publicKey }) {
  let lines
  let pub
  try {
    pub = parseMinisignPublicKey(publicKey)
    lines = decodeLines(signature, 'The signature')
  } catch (error) {
    return error.message
  }
  if (lines.length < 4 || !lines[0].startsWith('untrusted comment:') || !lines[2].startsWith('trusted comment: ')) {
    return 'the signature is not a minisign signature file'
  }
  const raw = Buffer.from(lines[1], 'base64')
  if (raw.length !== 74) return 'the signature line has the wrong length'
  const algorithm = raw.subarray(0, 2).toString('latin1')
  if (algorithm !== 'ED' && algorithm !== 'Ed') return `unknown signature algorithm ${algorithm}`
  if (!raw.subarray(2, 10).equals(pub.keyId)) {
    const id = (bytes) => Buffer.from(bytes).reverse().toString('hex').toUpperCase()
    return `signed by key ${id(raw.subarray(2, 10))}, not the shipped key ${id(pub.keyId)}`
  }
  const signatureBytes = raw.subarray(10)
  // "ED" signs the BLAKE2b-512 of the file (what Tauri writes), "Ed" the file.
  const message = algorithm === 'ED' ? createHash('blake2b512').update(data).digest() : Buffer.from(data)
  if (!verify(null, message, pub.key, signatureBytes)) return 'the signature does not match the file'
  const trustedComment = lines[2].slice('trusted comment: '.length)
  const globalSignature = Buffer.from(lines[3], 'base64')
  if (!verify(null, Buffer.concat([signatureBytes, Buffer.from(trustedComment, 'utf8')]), pub.key, globalSignature)) {
    return 'the trusted comment signature does not match'
  }
  return null
}

// Sparkle's EdDSA: a bare Ed25519 signature over the archive, both halves in
// base64, the public key being SUPublicEDKey from the app's Info.plist.
export function ed25519Problem({ data, signature, publicKey }) {
  const raw = Buffer.from(String(publicKey ?? ''), 'base64')
  if (raw.length !== 32) return 'the public EdDSA key is not 32 bytes of base64'
  const signatureBytes = Buffer.from(String(signature ?? ''), 'base64')
  if (signatureBytes.length !== 64) return 'the EdDSA signature is not 64 bytes of base64'
  const key = createPublicKey({ key: Buffer.concat([ED25519_SPKI_PREFIX, raw]), format: 'der', type: 'spki' })
  return verify(null, data, key, signatureBytes) ? null : 'the EdDSA signature does not match the archive'
}
