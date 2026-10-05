// node --test scripts/release/release-lib.test.mjs  (npm run test:release)

import assert from 'node:assert/strict'
import { test } from 'node:test'

import {
  anonymousGet,
  buildReleaseNotes,
  channelForVersion,
  checkManifest,
  checkPublicRelease,
  feedTags,
  hostArchiveName,
  macAppFileName,
  macAppNameFromXcconfig,
  macServerName,
  manifestFragment,
  mergeManifestFragments,
  missingInstallers,
  NIGHTLIES_KEPT,
  nightliesToPrune,
  planCollect,
  prereleaseVersion,
  productSlug,
  releaseFileName,
  releasesRepoFromEndpoint,
  sourceShaFromBody,
  unsignedMacApps,
  updaterUrls,
  utcDateStamp,
  verifyPublicRelease,
} from './release-lib.mjs'

const SHA = 'a'.repeat(40)
const REPO = 'acme/fairspoken'
const SLUG = 'fairspoken'

test('channelForVersion: stable, nightly, and prereleases nobody could follow', () => {
  assert.equal(channelForVersion('0.4.0'), 'latest')
  assert.equal(channelForVersion('v0.4.1-nightly.20260911.3'), 'nightly')
  assert.equal(channelForVersion('0.4.0-nightly'), 'nightly')
  assert.throws(() => channelForVersion('0.4.0-beta.1'), /not a nightly version/)
  assert.throws(() => channelForVersion('0.4.0-nightlyish.1'), /not a nightly version/)
  assert.throws(() => channelForVersion('0.4.0-preview.20260923.7'), /not a nightly version/)
  assert.throws(() => channelForVersion('0.4'), /Not a release version/)
})

test('prereleaseVersion orders by date then run number, on a known train only', () => {
  assert.equal(prereleaseVersion('0.4.1', 'nightly', '20260911', 42), '0.4.1-nightly.20260911.42')
  assert.throws(() => prereleaseVersion('0.4.1', 'beta', '20260911', 1), /Unknown prerelease train/)
  assert.throws(() => prereleaseVersion('0.4.1', 'nightly', '2026-09-11', 1), /YYYYMMDD/)
  assert.throws(() => prereleaseVersion('0.4.1', 'nightly', '20260911', 0), /positive integer/)
  assert.equal(utcDateStamp('2026-09-11T23:59:00Z'), '20260911')
})

test('the releases repository comes from the stable updater endpoint and nowhere else', () => {
  assert.equal(releasesRepoFromEndpoint('https://github.com/acme/multi-voice.app/releases/latest/download/latest.json'), 'acme/multi-voice.app')
  for (const wrong of [
    undefined,
    'https://github.com/acme/fairspoken/releases/download/nightly/latest.json',
    'https://example.com/acme/fairspoken/releases/latest/download/latest.json',
    'https://github.com/acme/fairspoken/releases/latest/download/latest.yml',
  ]) {
    assert.throws(() => releasesRepoFromEndpoint(wrong), /updater endpoint must be/)
  }
})

test('file names carry no spaces, whatever the product is called', () => {
  assert.equal(productSlug('Fairspoken Desktop'), 'fairspoken-desktop')
  assert.equal(productSlug('  Hush!  '), 'hush')
  assert.throws(() => productSlug('***'), /Cannot name files/)
})

test('release notes say which train a build is on', () => {
  const notes = (channel) =>
    buildReleaseNotes({ productName: 'Fairspoken', version: '0.5.0', channel, sourceRepo: 'o/r', sha: SHA, sourcePrivate: true })
  assert.match(notes('nightly'), /^Nightly build of Fairspoken 0\.5\.0\./)
  assert.match(notes('latest'), /^Fairspoken 0\.5\.0\.\n/)
})

test('the source sha round-trips through the release body', () => {
  const body = buildReleaseNotes({ productName: 'Fairspoken', version: '0.4.0', channel: 'latest', sourceRepo: 'o/r', sha: SHA, sourcePrivate: true })
  assert.equal(sourceShaFromBody(body), SHA)
  assert.equal(sourceShaFromBody('no marker here'), null)
  assert.equal(sourceShaFromBody(null), null)
})

test('release notes list commit subjects only when the source is public', () => {
  const commits = [{ sha: 'b'.repeat(40), subject: 'Private subject line' }]
  const base = { productName: 'Fairspoken', version: '0.4.0', channel: 'latest', sourceRepo: 'o/r', sha: SHA, commits }
  const privateBody = buildReleaseNotes({ ...base, sourcePrivate: true })
  assert.ok(!privateBody.includes('Private subject line'))
  assert.ok(!privateBody.includes('github.com/o/r'))
  const publicBody = buildReleaseNotes({ ...base, sourcePrivate: false })
  assert.ok(publicBody.includes('- Private subject line'))
})

// What `tauri build` leaves under target/release/bundle on each OS that ships
// the Tauri app. macOS is not one of them: Macs get the native apps.
const BUNDLES = {
  'windows-x86_64': ['bundle/nsis/Fairspoken_0.4.0_x64-setup.exe', 'bundle/nsis/Fairspoken_0.4.0_x64-setup.exe.sig'],
  'linux-x86_64': [
    'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage',
    'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage.sig',
    'bundle/deb/Fairspoken_0.4.0_amd64.deb',
  ],
}

test('each leg renames its bundles so every leg can share one release', () => {
  const plan = (platform) => planCollect({ files: BUNDLES[platform], platform, slug: SLUG, version: '0.4.0' })
  const linux = plan('linux-x86_64')
  assert.deepEqual(linux.copies, [
    { from: 'bundle/deb/Fairspoken_0.4.0_amd64.deb', to: 'fairspoken-0.4.0-linux-x86_64.deb' },
    { from: 'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage', to: 'fairspoken-0.4.0-linux-x86_64.AppImage' },
  ])
  assert.equal(linux.signature, 'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage.sig')
  assert.equal(linux.payloadName, 'fairspoken-0.4.0-linux-x86_64.AppImage')
  assert.deepEqual(plan('windows-x86_64').copies.map((copy) => copy.to), ['fairspoken-0.4.0-windows-x64-setup.exe'])
  assert.equal(plan('windows-x86_64').payloadName, 'fairspoken-0.4.0-windows-x64-setup.exe')
})

test('a leg that cannot say which file it built, or built it unsigned, fails', () => {
  const unsigned = BUNDLES['linux-x86_64'].filter((file) => !file.endsWith('.sig'))
  assert.throws(
    () => planCollect({ files: unsigned, platform: 'linux-x86_64', slug: SLUG, version: '0.4.0' }),
    /has no signature.*TAURI_SIGNING_PRIVATE_KEY/,
  )
  const twoDebs = [...BUNDLES['linux-x86_64'], 'bundle/deb/stale.deb']
  assert.throws(() => planCollect({ files: twoDebs, platform: 'linux-x86_64', slug: SLUG, version: '0.4.0' }), /Expected one deb.*found 2/)
  assert.throws(() => planCollect({ files: [], platform: 'linux-aarch64', slug: SLUG, version: '0.4.0' }), /Unknown updater platform/)
})

test('the Tauri app has no macOS platform: no leg can collect one, and no manifest entry is written for it', () => {
  const macBundle = [
    'bundle/dmg/Fairspoken_0.4.0_aarch64.dmg',
    'bundle/macos/Fairspoken.app.tar.gz',
    'bundle/macos/Fairspoken.app.tar.gz.sig',
  ]
  for (const platform of ['darwin-aarch64', 'darwin-x86_64']) {
    assert.throws(() => planCollect({ files: macBundle, platform, slug: SLUG, version: '0.4.0' }), /Unknown updater platform/)
    assert.throws(
      () => manifestFragment({ platform, version: '0.4.0', repo: REPO, tag: 'v0.4.0', payloadName: 'x.app.tar.gz', signature: 'sig' }),
      /Unknown updater platform/,
    )
    assert.throws(() => releaseFileName({ slug: SLUG, version: '0.4.0', platform, suffix: '.dmg' }), /Unknown updater platform/)
  }
})

const TAG = 'v0.4.0'
const RELEASE_ASSETS = [
  'fairspoken-0.4.0-windows-x64-setup.exe',
  'fairspoken-0.4.0-linux-x86_64.AppImage',
  'fairspoken-0.4.0-linux-x86_64.deb',
  'fairspoken-0.4.0-transcription-host-linux-x64.tar.gz',
  'fairspoken-0.4.0-transcription-host-linux-arm64.tar.gz',
  'fairspoken-0.4.0-transcription-host-windows-x64.zip',
  'fairspoken-0.4.0-transcription-host-macos-arm64.tar.gz',
  'fairspoken-0.4.0-transcription-host-macos-x64.tar.gz',
  'Fairspoken-0.4.0-macos-arm64.zip',
  'Fairspoken-0.4.0-macos-arm64.dmg',
  'Fairspoken-Server-0.4.0-macos-arm64.zip',
  'Fairspoken-Server-0.4.0-macos-arm64.dmg',
]

function fragments(version = '0.4.0', tag = TAG) {
  const payloads = {
    'windows-x86_64': `fairspoken-${version}-windows-x64-setup.exe`,
    'linux-x86_64': `fairspoken-${version}-linux-x86_64.AppImage`,
  }
  return Object.entries(payloads).map(([platform, payloadName]) =>
    manifestFragment({ platform, version, repo: REPO, tag, payloadName, signature: `sig-${platform}\n` }),
  )
}

const manifestText = (version = '0.4.0', tag = TAG) =>
  JSON.stringify(mergeManifestFragments(fragments(version, tag), { notes: 'n', pubDate: '2026-09-11T00:00:00.000Z' }))

test('the merged manifest carries every platform, and passes the updater check', () => {
  const manifest = JSON.parse(manifestText())
  assert.equal(manifest.version, '0.4.0')
  assert.deepEqual(Object.keys(manifest.platforms), ['windows-x86_64', 'linux-x86_64'])
  assert.equal(manifest.platforms['windows-x86_64'].signature, 'sig-windows-x86_64')
  assert.equal(
    manifest.platforms['linux-x86_64'].url,
    `https://github.com/${REPO}/releases/download/v0.4.0/fairspoken-0.4.0-linux-x86_64.AppImage`,
  )
  assert.deepEqual(checkManifest(manifestText(), { version: '0.4.0', repo: REPO, tag: TAG, assetNames: RELEASE_ASSETS }), [])
})

test('merging refuses fragments that disagree or overlap', () => {
  const [windows, linux] = fragments()
  assert.throws(() => mergeManifestFragments([windows, { ...linux, version: '0.4.1' }], {}), /disagree on version/)
  assert.throws(() => mergeManifestFragments([windows, windows], {}), /both claim windows-x86_64/)
  assert.throws(() => mergeManifestFragments([], {}), /No updater manifest fragments/)
  assert.throws(
    () => manifestFragment({ platform: 'linux-x86_64', version: '0.4.0', repo: REPO, tag: TAG, payloadName: 'x', signature: ' \n' }),
    /signature is empty/,
  )
})

test('checkManifest names what a partial or misdirected manifest lacks', () => {
  const check = (text, overrides = {}) =>
    checkManifest(text, { version: '0.4.0', repo: REPO, tag: TAG, assetNames: RELEASE_ASSETS, ...overrides })
  const windowsOnly = JSON.stringify(mergeManifestFragments(fragments().slice(0, 1), { pubDate: 'x' }))
  assert.deepEqual(check(windowsOnly), ['has no linux-x86_64 entry, so those installs are never offered it'])
  assert.deepEqual(check(manifestText(), { version: '0.4.1' }), ['names version 0.4.0, expected 0.4.1'])
  const elsewhere = manifestText().replaceAll(`github.com/${REPO}/`, 'github.com/acme/other/')
  assert.equal(check(elsewhere).length, 2)
  assert.match(check(elsewhere)[0], /not at v0\.4\.0 on acme\/fairspoken/)
  const missingAsset = check(manifestText(), { assetNames: RELEASE_ASSETS.filter((name) => !name.endsWith('.AppImage')) })
  assert.deepEqual(missingAsset, ['points linux-x86_64 at fairspoken-0.4.0-linux-x86_64.AppImage, which is not on the release'])
  const unsigned = JSON.parse(manifestText())
  unsigned.platforms['windows-x86_64'].signature = ''
  assert.deepEqual(check(JSON.stringify(unsigned)), ['has no windows-x86_64 signature, and the updater refuses an unsigned payload'])
  assert.match(check('version: 0.4.0')[0], /does not parse/)
})

test('missingInstallers wants every product: the Tauri installers, each host archive and both native apps', () => {
  const options = { slug: SLUG, version: '0.4.0', macAppName: 'Fairspoken' }
  assert.deepEqual(missingInstallers(RELEASE_ASSETS, options), [])
  assert.deepEqual(missingInstallers([], options), [
    'a Windows -setup.exe (fairspoken-0.4.0-windows-x64-setup.exe)',
    'an .AppImage (fairspoken-0.4.0-linux-x86_64.AppImage)',
    'the linux-x64 transcription host (fairspoken-0.4.0-transcription-host-linux-x64.tar.gz)',
    'the linux-arm64 transcription host (fairspoken-0.4.0-transcription-host-linux-arm64.tar.gz)',
    'the windows-x64 transcription host (fairspoken-0.4.0-transcription-host-windows-x64.zip)',
    'the macos-arm64 transcription host (fairspoken-0.4.0-transcription-host-macos-arm64.tar.gz)',
    'the macos-x64 transcription host (fairspoken-0.4.0-transcription-host-macos-x64.tar.gz)',
    'the native macOS app .zip (Fairspoken-0.4.0-macos-arm64.zip)',
    'the native macOS app .dmg (Fairspoken-0.4.0-macos-arm64.dmg)',
    'the native macOS server app .zip (Fairspoken-Server-0.4.0-macos-arm64.zip)',
    'the native macOS server app .dmg (Fairspoken-Server-0.4.0-macos-arm64.dmg)',
  ])
  // A commit from before apps/macos existed ships no native app.
  assert.equal(missingInstallers(RELEASE_ASSETS, { ...options, macAppName: null }).length, 0)
  assert.equal(missingInstallers([], { ...options, macAppName: null }).length, 7)
})

test('the Tauri macOS app is no longer asked for, and its files do not stand in for anything', () => {
  const options = { slug: SLUG, version: '0.4.0', macAppName: 'Fairspoken' }
  // The current asset set carries no Tauri macOS DMG or updater payload.
  assert.ok(!RELEASE_ASSETS.some((name) => /^fairspoken-.*-macos-(arm64|x64)(\.dmg|\.app\.tar\.gz)$/.test(name)))
  assert.deepEqual(missingInstallers(RELEASE_ASSETS, options), [])
  // The macOS host archives are still required.
  for (const target of ['macos-arm64', 'macos-x64']) {
    const name = `fairspoken-0.4.0-transcription-host-${target}.tar.gz`
    assert.deepEqual(missingInstallers(RELEASE_ASSETS.filter((asset) => asset !== name), options), [
      `the ${target} transcription host (${name})`,
    ])
  }
  // An old-style Tauri Apple Silicon DMG is not the native app.
  const tauriDmgOnly = [
    ...RELEASE_ASSETS.filter((name) => name !== 'Fairspoken-0.4.0-macos-arm64.dmg'),
    'fairspoken-0.4.0-macos-arm64.dmg',
  ]
  assert.deepEqual(missingInstallers(tauriDmgOnly, options), ['the native macOS app .dmg (Fairspoken-0.4.0-macos-arm64.dmg)'])
})

test("the client's and the server's packages never stand in for each other", () => {
  const options = { slug: SLUG, version: '0.4.0', macAppName: 'Fairspoken' }
  const withoutClient = RELEASE_ASSETS.filter((name) => !name.startsWith('Fairspoken-0.4.0-'))
  assert.deepEqual(missingInstallers(withoutClient, options), [
    'the native macOS app .zip (Fairspoken-0.4.0-macos-arm64.zip)',
    'the native macOS app .dmg (Fairspoken-0.4.0-macos-arm64.dmg)',
  ])
  const withoutServer = RELEASE_ASSETS.filter((name) => !name.startsWith('Fairspoken-Server-'))
  assert.deepEqual(missingInstallers(withoutServer, options), [
    'the native macOS server app .zip (Fairspoken-Server-0.4.0-macos-arm64.zip)',
    'the native macOS server app .dmg (Fairspoken-Server-0.4.0-macos-arm64.dmg)',
  ])
  const serverDmgOnly = RELEASE_ASSETS.filter((name) => name !== 'Fairspoken-Server-0.4.0-macos-arm64.dmg')
  assert.deepEqual(missingInstallers(serverDmgOnly, options), [
    'the native macOS server app .dmg (Fairspoken-Server-0.4.0-macos-arm64.dmg)',
  ])
})

test('ad-hoc signed native apps complete a release, and are named as such', () => {
  const options = { slug: SLUG, version: '0.4.0', macAppName: 'Fairspoken' }
  const unsigned = RELEASE_ASSETS.map((name) => name.replace(/^(Fairspoken(?:-Server)?-0\.4\.0-macos-arm64)/, '$1-unsigned'))
  assert.deepEqual(missingInstallers(unsigned, options), [])
  assert.deepEqual(unsignedMacApps(unsigned, options), [
    'Fairspoken-0.4.0-macos-arm64-unsigned.zip',
    'Fairspoken-0.4.0-macos-arm64-unsigned.dmg',
    'Fairspoken-Server-0.4.0-macos-arm64-unsigned.zip',
    'Fairspoken-Server-0.4.0-macos-arm64-unsigned.dmg',
  ])
  assert.deepEqual(unsignedMacApps(RELEASE_ASSETS, options), [])
  assert.deepEqual(unsignedMacApps(unsigned, { ...options, macAppName: null }), [])
  // Only the server unsigned: only the server is named.
  const serverUnsigned = RELEASE_ASSETS.map((name) => name.replace(/^(Fairspoken-Server-0\.4\.0-macos-arm64)/, '$1-unsigned'))
  assert.deepEqual(unsignedMacApps(serverUnsigned, options), [
    'Fairspoken-Server-0.4.0-macos-arm64-unsigned.zip',
    'Fairspoken-Server-0.4.0-macos-arm64-unsigned.dmg',
  ])
  // A local signing check (scripts/release.sh --no-notarize) is not a release file.
  const unnotarized = RELEASE_ASSETS.map((name) => name.replace(/^(Fairspoken(?:-Server)?-0\.4\.0-macos-arm64)/, '$1-unnotarized'))
  assert.equal(missingInstallers(unnotarized, options).length, 4)
})

test('host archives and native app packages are named per platform, with no spaces', () => {
  assert.equal(
    hostArchiveName({ slug: SLUG, version: '0.5.0-nightly.20261004.41', target: 'linux-arm64' }),
    'fairspoken-0.5.0-nightly.20261004.41-transcription-host-linux-arm64.tar.gz',
  )
  assert.equal(hostArchiveName({ slug: SLUG, version: '0.5.0', target: 'windows-x64' }), 'fairspoken-0.5.0-transcription-host-windows-x64.zip')
  assert.throws(() => hostArchiveName({ slug: SLUG, version: '0.5.0', target: 'linux-riscv64' }), /Unknown transcription host target/)
  assert.equal(
    macAppFileName({ appName: 'Fairspoken', version: '0.5.0-nightly.20261004.41', extension: '.dmg' }),
    'Fairspoken-0.5.0-nightly.20261004.41-macos-arm64.dmg',
  )
  assert.equal(
    macAppFileName({ appName: 'Fairspoken', version: '0.5.0', signed: false, extension: '.zip' }),
    'Fairspoken-0.5.0-macos-arm64-unsigned.zip',
  )
  assert.throws(() => macAppFileName({ appName: 'Fair spoken', version: '0.5.0', extension: '.zip' }), /Cannot name files/)
  assert.throws(() => macAppFileName({ appName: 'Fairspoken', version: '0.5.0', extension: '.pkg' }), /Unknown macOS app package/)
  assert.equal(macServerName('Fairspoken'), 'Fairspoken-Server')
  assert.equal(macServerName(null), null)
  assert.equal(
    macAppFileName({ appName: macServerName('Fairspoken'), version: '0.5.0-nightly.20261004.41', signed: false, extension: '.dmg' }),
    'Fairspoken-Server-0.5.0-nightly.20261004.41-macos-arm64-unsigned.dmg',
  )
})

test("the native app's name is read from MV_DISPLAY_NAME in its xcconfig", () => {
  const xcconfig = '// PRODUCT NAME\nMV_DISPLAY_NAME = Fairspoken\nMV_BUNDLE_ID_BASE = ie.fairspoken.mac\n'
  assert.equal(macAppNameFromXcconfig(xcconfig), 'Fairspoken')
  assert.equal(macAppNameFromXcconfig('MARKETING_VERSION = 0.1.0\n'), null)
  assert.equal(macAppNameFromXcconfig(undefined), null)
})

// The unauthenticated half of `release.mjs verify`. Every request is served
// from a fixture: these tests describe what a user's updater would see, and a
// test that needed the network could not run on a release that is broken.

const ok = (text) => ({ ok: true, status: 200, text })

function atomFeed(tags) {
  const entries = tags.map(
    (tag) =>
      `  <entry>\n    <title>${tag}</title>\n` +
      `    <link rel="alternate" type="text/html" href="https://github.com/${REPO}/releases/tag/${encodeURIComponent(tag)}"/>\n` +
      `  </entry>`,
  )
  return `<?xml version="1.0" encoding="UTF-8"?>\n<feed>\n${entries.join('\n')}\n</feed>\n`
}

// Anything not published answers 404, which is what GitHub does.
const servedBy = (responses) => async (url) => responses[url] ?? { ok: false, status: 404, text: '' }

// A release as it stands on the repository: the feed it appears in, the tag
// /releases/latest resolves to, its manifest, and what the stable endpoint
// serves. Defaults describe a healthy publish; every test breaks one thing.
function published(version, { feed, latest } = {}) {
  const tag = `v${version}`
  const channel = channelForVersion(version)
  const assetNames = RELEASE_ASSETS.map((name) => name.replace('0.4.0', version))
  const urls = updaterUrls({ repo: REPO, tag, channel })
  const pointer = latest === undefined ? (channel === 'latest' ? tag : null) : latest
  const responses = {
    [urls.feed]: ok(atomFeed(feed ?? [tag])),
    [urls.manifest]: ok(manifestText(version, tag)),
  }
  if (pointer) {
    responses[urls.latestPointer] = ok(JSON.stringify({ tag_name: pointer }))
    const pointerVersion = pointer.replace(/^v/, '')
    responses[urls.stableManifest] = ok(manifestText(pointerVersion, pointer))
  }
  const world = { tag, channel, version, assetNames, urls, responses }
  world.check = () => checkPublicRelease({ repo: REPO, tag, channel, version, assetNames, get: servedBy(responses) })
  return world
}

test('anonymousGet sends no credential, whatever is in the environment', async () => {
  const sent = []
  const previousToken = process.env.GH_TOKEN
  process.env.GH_TOKEN = 'ghp_not_a_real_token'
  try {
    const get = anonymousGet(async (url, init) => {
      sent.push({ url, headers: init.headers })
      return { ok: true, status: 200, text: async () => 'body' }
    })
    assert.deepEqual(await get(`https://github.com/${REPO}/releases.atom`, { accept: 'application/xml' }), {
      ok: true,
      status: 200,
      text: 'body',
    })
  } finally {
    if (previousToken === undefined) delete process.env.GH_TOKEN
    else process.env.GH_TOKEN = previousToken
  }
  const headerNames = Object.keys(sent[0].headers).map((name) => name.toLowerCase())
  assert.deepEqual(headerNames, ['accept', 'user-agent'])
  assert.ok(!JSON.stringify(sent[0]).includes('ghp_not_a_real_token'))
})

test('anonymousGet reports a refused connection rather than throwing', async () => {
  const get = anonymousGet(async () => {
    throw new Error('getaddrinfo ENOTFOUND github.com')
  })
  const response = await get(`https://github.com/${REPO}/releases.atom`)
  assert.equal(response.ok, false)
  assert.equal(response.status, 0)
  assert.match(response.error, /ENOTFOUND/)
})

test('updaterUrls names what installed builds read on each channel', () => {
  const stable = updaterUrls({ repo: REPO, tag: 'v0.4.0', channel: 'latest' })
  assert.equal(stable.feed, `https://github.com/${REPO}/releases.atom`)
  assert.equal(stable.latestPointer, `https://github.com/${REPO}/releases/latest`)
  assert.equal(stable.stableManifest, `https://github.com/${REPO}/releases/latest/download/latest.json`)
  assert.equal(stable.manifest, `https://github.com/${REPO}/releases/download/v0.4.0/latest.json`)
  const nightly = updaterUrls({ repo: REPO, tag: 'v0.4.1-nightly.20260911.3', channel: 'nightly' })
  assert.equal(nightly.manifest, `https://github.com/${REPO}/releases/download/v0.4.1-nightly.20260911.3/nightly.json`)
})

test('feedTags reads the feed newest first, as the app does', () => {
  assert.deepEqual(feedTags(atomFeed(['v0.4.1-nightly.20260911.3', 'v0.4.0'])), ['v0.4.1-nightly.20260911.3', 'v0.4.0'])
  assert.deepEqual(feedTags(atomFeed(['app@0.4.0'])), ['app@0.4.0'])
  assert.deepEqual(feedTags('<feed></feed>'), [])
})

test('a complete public release, stable and nightly, has nothing to report', async () => {
  assert.deepEqual((await published('0.4.0').check()).problems, [])
  // A stable release is resolved through /releases/latest, so nightlies sitting
  // above it in the feed are none of its business.
  const stableUnderNightlies = published('0.4.0', { feed: ['v0.4.1-nightly.20260912.9', 'v0.4.0'] })
  assert.deepEqual((await stableUnderNightlies.check()).problems, [])
  const nightly = published('0.4.1-nightly.20260911.3', {
    feed: ['v0.4.1-nightly.20260911.3', 'v0.4.0'],
    latest: 'v0.4.0',
  })
  assert.deepEqual((await nightly.check()).problems, [])
})

test('a repository no user can read fails at once, and says what to look at', async () => {
  const waits = []
  const problems = await verifyPublicRelease({
    repo: REPO,
    tag: 'v0.4.0',
    channel: 'latest',
    version: '0.4.0',
    assetNames: RELEASE_ASSETS,
    get: servedBy({}),
    attempts: 5,
    delayMs: 1,
    wait: async (ms) => waits.push(ms),
  })
  assert.equal(problems.length, 1)
  assert.match(problems[0], /releases\.atom answered HTTP 404 without credentials/)
  assert.match(problems[0], /is public/)
  assert.match(problems[0], /tauri\.conf\.json/)
  // Waiting cannot make a private repository public.
  assert.deepEqual(waits, [])
})

test('a release the feed has not caught up with is polled for, not failed on', async () => {
  const world = published('0.4.0', { feed: [] })
  const waits = []
  const problems = await verifyPublicRelease({
    repo: REPO,
    tag: world.tag,
    channel: world.channel,
    version: world.version,
    assetNames: world.assetNames,
    get: servedBy(world.responses),
    attempts: 3,
    delayMs: 1,
    wait: async (ms) => {
      waits.push(ms)
      world.responses[world.urls.feed] = ok(atomFeed([world.tag]))
    },
  })
  assert.deepEqual(problems, [])
  assert.deepEqual(waits, [1])
})

test('a release missing from the feed after every attempt is reported with what the feed does list', async () => {
  const world = published('0.4.0', { feed: ['v0.3.9'] })
  const problems = await verifyPublicRelease({
    repo: REPO,
    tag: world.tag,
    channel: world.channel,
    version: world.version,
    assetNames: world.assetNames,
    get: servedBy(world.responses),
    attempts: 2,
    delayMs: 1,
    wait: async () => {},
  })
  assert.equal(problems.length, 1)
  assert.match(problems[0], /v0\.4\.0 is not in .*releases\.atom, which lists v0\.3\.9/)
  assert.match(problems[0], /draft/)
})

test('the stable pointer must name the release, and must not name a nightly', async () => {
  const stale = await published('0.4.0', { latest: 'v0.3.9' }).check()
  assert.equal(stale.problems.length, 2)
  assert.match(stale.problems[0], /resolves to v0\.3\.9, not v0\.4\.0/)
  assert.match(stale.problems[0], /gh release edit v0\.4\.0 --latest/)
  // The stable endpoint still serving the previous manifest is the same lag.
  assert.match(stale.problems[1], /latest\/download\/latest\.json names version 0\.3\.9, expected 0\.4\.0/)
  assert.equal(stale.retryable, true)

  const nightlyTag = 'v0.4.1-nightly.20260911.3'
  const mislabelled = await published('0.4.1-nightly.20260911.3', { feed: [nightlyTag], latest: nightlyTag }).check()
  assert.equal(mislabelled.problems.length, 1)
  assert.match(mislabelled.problems[0], /resolves to v0\.4\.1-nightly\.20260911\.3, a nightly/)
  assert.match(mislabelled.problems[0], /--latest=false/)
  // A nightly that stable users could be offered is a mistake waiting will not
  // undo, so it is not retried.
  assert.equal(mislabelled.retryable, false)
})

test('a nightly behind a newer nightly in the feed reaches nobody', async () => {
  const world = published('0.4.1-nightly.20260911.3', {
    feed: ['v0.4.1-nightly.20260912.9', 'v0.4.1-nightly.20260911.3', 'v0.4.0'],
  })
  const { problems, retryable } = await world.check()
  assert.equal(problems.length, 1)
  assert.match(problems[0], /lists v0\.4\.1-nightly\.20260912\.9 above v0\.4\.1-nightly\.20260911\.3/)
  assert.equal(retryable, false)
})

test('a manifest the release page will not serve is named', async () => {
  const world = published('0.4.1-nightly.20260911.3')
  delete world.responses[world.urls.manifest]
  const { problems, retryable } = await world.check()
  assert.equal(problems.length, 1)
  assert.match(problems[0], /nightly\.json answered HTTP 404 without credentials/)
  assert.equal(retryable, true)
})

test('a manifest served to users is checked, not only the copy the token can read', async () => {
  const world = published('0.4.0')
  const partial = JSON.parse(manifestText())
  delete partial.platforms['linux-x86_64']
  world.responses[world.urls.manifest] = ok(JSON.stringify(partial))
  const { problems, retryable } = await world.check()
  assert.deepEqual(problems, [`${world.urls.manifest} has no linux-x86_64 entry, so those installs are never offered it`])
  assert.equal(retryable, false)
})

test('only nightlies past the newest few are pruned, never a stable, a draft or a stranger', () => {
  const release = (tag_name, day, extra = {}) => ({ tag_name, published_at: `2026-09-${day}T12:00:00Z`, draft: false, ...extra })
  const releases = [
    release('v0.6.0', '22'),
    release('v0.7.0-nightly.20260923.28', '23'),
    // Published out of tag order: age is when it was published.
    release('v0.7.0-nightly.20260929.58', '29'),
    release('v0.7.0-nightly.20260924.31', '24'),
    release('v0.7.0-nightly.20260928.54', '28'),
    release('v0.7.0-nightly.20260925.35', '25'),
    release('v0.7.0-nightly.20260930.60', '30', { draft: true }),
    release('v0.7.0-nightly.20260930.61', '30', { published_at: null }),
    release('v0.4.0-beta.20260919.2', '19'),
    release('some-other-tag', '01'),
  ]
  assert.equal(NIGHTLIES_KEPT, 3)
  assert.deepEqual(nightliesToPrune(releases), ['v0.7.0-nightly.20260924.31', 'v0.7.0-nightly.20260923.28'])
  assert.deepEqual(nightliesToPrune(releases, 5), [])
  assert.deepEqual(nightliesToPrune([]), [])
  assert.throws(() => nightliesToPrune(releases, 0), /Keep at least one nightly/)
})
