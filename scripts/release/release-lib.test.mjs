// node --test scripts/release/release-lib.test.mjs  (npm run test:release)

import assert from 'node:assert/strict'
import { test } from 'node:test'

import {
  anonymousGet,
  buildNumber,
  buildReleaseNotes,
  channelForVersion,
  checkDesktopManifest,
  checkHostManifest,
  checkSecrets,
  checkSparkleMetadata,
  DESKTOP_MANIFEST,
  DESKTOP_PLATFORMS,
  desktopFragment,
  HOST_MANIFEST,
  HOST_PLATFORMS,
  hostArchiveLayout,
  hostArchiveName,
  hostFragment,
  isBuildNumber,
  macAppFileName,
  macAppNameFromXcconfig,
  macAppsWithoutSparkle,
  macServerName,
  mergeFragments,
  missingInstallers,
  NIGHTLIES_KEPT,
  nightliesToPrune,
  planCollect,
  prereleaseVersion,
  productSlug,
  releaseFileName,
  releasesRepoFromEndpoint,
  sourceShaFromBody,
  SPARKLE_APPS,
  unsignedMacApps,
  utcDateStamp,
} from './release-lib.mjs'

const SHA = 'a'.repeat(40)
const REPO = 'acme/fairspoken'
const SLUG = 'fairspoken'

test('channelForVersion: stable, nightly, and prereleases nobody could follow', () => {
  assert.equal(channelForVersion('0.4.0'), 'stable')
  assert.equal(channelForVersion('v0.4.1-nightly.20260911.3'), 'nightly')
  assert.equal(channelForVersion('0.4.0-nightly'), 'nightly')
  assert.throws(() => channelForVersion('0.4.0-beta.1'), /not a nightly version/)
  assert.throws(() => channelForVersion('0.4.0-nightlyish.1'), /not a nightly version/)
  assert.throws(() => channelForVersion('0.4.0-preview.20260923.7'), /not a nightly version/)
  assert.throws(() => channelForVersion('0.4'), /Not a release version/)
  assert.throws(() => channelForVersion('update-feeds'), /Not a release version/)
})

test('prereleaseVersion orders by date then run number, on a known train only', () => {
  assert.equal(prereleaseVersion('0.4.1', 'nightly', '20260911', 42), '0.4.1-nightly.20260911.42')
  assert.throws(() => prereleaseVersion('0.4.1', 'beta', '20260911', 1), /Unknown prerelease train/)
  assert.throws(() => prereleaseVersion('0.4.1', 'nightly', '2026-09-11', 1), /YYYYMMDD/)
  assert.throws(() => prereleaseVersion('0.4.1', 'nightly', '20260911', 0), /positive integer/)
  assert.equal(utcDateStamp('2026-09-11T23:59:00Z'), '20260911')
})

test('the build number is the UTC minute, twelve digits, and only grows', () => {
  assert.equal(buildNumber('2026-10-05T12:34:56.789Z'), '202610051234')
  // Local offsets are folded into UTC: the same instant, the same number.
  assert.equal(buildNumber('2026-10-05T13:34:00+01:00'), '202610051234')
  assert.equal(buildNumber('2026-01-02T03:04:00Z'), '202601020304')
  assert.throws(() => buildNumber('yesterday'), /Not a timestamp/)
  // A promotion is resolved after the nightly it promotes, so it sorts above.
  const nightly = buildNumber('2026-10-04T23:59:00Z')
  const stable = buildNumber('2026-10-05T00:00:00Z')
  assert.ok(Number(stable) > Number(nightly))
  // Bigger than any run number this repository will reach, and well inside a
  // double and Sparkle's long long comparison.
  assert.ok(Number.isSafeInteger(Number(stable)))
  assert.ok(isBuildNumber('202610051234'))
  for (const wrong of ['20261005123', '2026100512345', '202613051234', '202610052460', '41', '', undefined]) {
    assert.equal(isBuildNumber(wrong), false, String(wrong))
  }
})

test('the releases repository comes from the stable desktop feed endpoint and nowhere else', () => {
  assert.equal(
    releasesRepoFromEndpoint('https://github.com/sprintengine/fairspoken/releases/download/update-feeds/desktop-stable.json'),
    'sprintengine/fairspoken',
  )
  assert.equal(releasesRepoFromEndpoint('https://github.com/acme/multi-voice.app/releases/download/update-feeds/desktop-stable.json'), 'acme/multi-voice.app')
  for (const wrong of [
    undefined,
    'https://github.com/acme/fairspoken/releases/latest/download/latest.json',
    'https://github.com/acme/fairspoken/releases/download/update-feeds/desktop-nightly.json',
    'https://github.com/acme/fairspoken/releases/download/v0.4.0/desktop-stable.json',
    'https://example.com/acme/fairspoken/releases/download/update-feeds/desktop-stable.json',
  ]) {
    assert.throws(() => releasesRepoFromEndpoint(wrong), /updater endpoint must be/)
  }
})

test('file names carry no spaces, whatever the product is called', () => {
  assert.equal(productSlug('Fairspoken'), 'fairspoken')
  assert.equal(productSlug('  Hush!  '), 'hush')
  assert.throws(() => productSlug('***'), /Cannot name files/)
})

test('release notes say which train a build is on', () => {
  const notes = (channel) =>
    buildReleaseNotes({ productName: 'Fairspoken', version: '0.5.0', channel, sourceRepo: 'o/r', sha: SHA, sourcePrivate: true })
  assert.match(notes('nightly'), /^Nightly build of Fairspoken 0\.5\.0\./)
  assert.match(notes('stable'), /^Fairspoken 0\.5\.0\.\n/)
})

test('the source sha round-trips through the release body', () => {
  const body = buildReleaseNotes({ productName: 'Fairspoken', version: '0.4.0', channel: 'stable', sourceRepo: 'o/r', sha: SHA, sourcePrivate: true })
  assert.equal(sourceShaFromBody(body), SHA)
  assert.equal(sourceShaFromBody('no marker here'), null)
  assert.equal(sourceShaFromBody(null), null)
})

test('release notes list commit subjects only when the source is public', () => {
  const commits = [{ sha: 'b'.repeat(40), subject: 'Private subject line' }]
  const base = { productName: 'Fairspoken', version: '0.4.0', channel: 'stable', sourceRepo: 'o/r', sha: SHA, commits }
  const privateBody = buildReleaseNotes({ ...base, sourcePrivate: true })
  assert.ok(!privateBody.includes('Private subject line'))
  assert.ok(!privateBody.includes('github.com/o/r'))
  const publicBody = buildReleaseNotes({ ...base, sourcePrivate: false })
  assert.ok(publicBody.includes('- Private subject line'))
})

// What `tauri build` leaves under target/release/bundle on each OS that ships
// the Tauri app (createUpdaterArtifacts on). macOS is not one of them.
const BUNDLES = {
  'windows-x86_64': ['bundle/nsis/Fairspoken_0.4.0_x64-setup.exe', 'bundle/nsis/Fairspoken_0.4.0_x64-setup.exe.sig'],
  'linux-x86_64': [
    'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage',
    'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage.sig',
    'bundle/deb/Fairspoken_0.4.0_amd64.deb',
    'bundle/deb/Fairspoken_0.4.0_amd64.deb.sig',
  ],
}

test('each leg renames its bundles, and the deb is an updater payload beside the AppImage', () => {
  const plan = (platform) => planCollect({ files: BUNDLES[platform], platform, slug: SLUG, version: '0.4.0' })
  const linux = plan('linux-x86_64')
  assert.deepEqual(linux.copies, [
    { from: 'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage', to: 'fairspoken-0.4.0-linux-x86_64.AppImage' },
    { from: 'bundle/deb/Fairspoken_0.4.0_amd64.deb', to: 'fairspoken-0.4.0-linux-x86_64.deb' },
  ])
  assert.deepEqual(
    linux.payloads.map(({ platform, name, signature }) => ({ platform, name, signature })),
    [
      { platform: 'linux-x86_64', name: 'fairspoken-0.4.0-linux-x86_64.AppImage', signature: 'bundle/appimage/Fairspoken_0.4.0_amd64.AppImage.sig' },
      { platform: 'linux-x86_64-deb', name: 'fairspoken-0.4.0-linux-x86_64.deb', signature: 'bundle/deb/Fairspoken_0.4.0_amd64.deb.sig' },
    ],
  )
  assert.deepEqual(plan('windows-x86_64').copies.map((copy) => copy.to), ['fairspoken-0.4.0-windows-x64-setup.exe'])
  assert.deepEqual(plan('windows-x86_64').payloads.map((payload) => payload.platform), ['windows-x86_64'])
  assert.deepEqual(Object.keys(DESKTOP_PLATFORMS), ['windows-x86_64', 'linux-x86_64', 'linux-x86_64-deb'])
})

test('a leg that cannot say which file it built, or built it unsigned, fails', () => {
  const unsignedDeb = BUNDLES['linux-x86_64'].filter((file) => !file.endsWith('.deb.sig'))
  assert.throws(
    () => planCollect({ files: unsignedDeb, platform: 'linux-x86_64', slug: SLUG, version: '0.4.0' }),
    /\.deb has no signature.*TAURI_SIGNING_PRIVATE_KEY/,
  )
  const twoDebs = [...BUNDLES['linux-x86_64'], 'bundle/deb/stale.deb']
  assert.throws(() => planCollect({ files: twoDebs, platform: 'linux-x86_64', slug: SLUG, version: '0.4.0' }), /Expected one deb.*found 2/)
  assert.throws(() => planCollect({ files: [], platform: 'linux-aarch64', slug: SLUG, version: '0.4.0' }), /Unknown desktop platform/)
})

test('the Tauri app has no macOS platform: no leg can collect one, and no manifest entry is written for it', () => {
  const macBundle = ['bundle/dmg/Fairspoken_0.4.0_aarch64.dmg', 'bundle/macos/Fairspoken.app.tar.gz', 'bundle/macos/Fairspoken.app.tar.gz.sig']
  for (const platform of ['darwin-aarch64', 'darwin-x86_64']) {
    assert.throws(() => planCollect({ files: macBundle, platform, slug: SLUG, version: '0.4.0' }), /Unknown desktop platform/)
    assert.throws(
      () => desktopFragment({ version: '0.4.0', repo: REPO, tag: 'v0.4.0', payloads: [{ platform, name: 'x.app.tar.gz', signature: 'sig' }] }),
      /Unknown desktop platform/,
    )
    assert.throws(() => releaseFileName({ slug: SLUG, version: '0.4.0', platform, suffix: '.dmg' }), /Unknown desktop platform/)
  }
})

const TAG = 'v0.4.0'

function releaseAssets(version = '0.4.0') {
  return [
    `fairspoken-${version}-windows-x64-setup.exe`,
    `fairspoken-${version}-linux-x86_64.AppImage`,
    `fairspoken-${version}-linux-x86_64.deb`,
    DESKTOP_MANIFEST,
    `fairspoken-${version}-transcription-host-linux-x64.tar.gz`,
    `fairspoken-${version}-transcription-host-linux-arm64.tar.gz`,
    `fairspoken-${version}-transcription-host-windows-x64.zip`,
    `fairspoken-${version}-transcription-host-macos-arm64.tar.gz`,
    HOST_MANIFEST,
    `Fairspoken-${version}-macos-arm64.zip`,
    `Fairspoken-${version}-macos-arm64.dmg`,
    `Fairspoken-Server-${version}-macos-arm64.zip`,
    `Fairspoken-Server-${version}-macos-arm64.dmg`,
  ]
}
const RELEASE_ASSETS = releaseAssets()

function desktopManifest(version = '0.4.0', tag = TAG) {
  const fragments = [
    desktopFragment({
      version,
      repo: REPO,
      tag,
      payloads: [{ platform: 'windows-x86_64', name: `fairspoken-${version}-windows-x64-setup.exe`, signature: 'sig-windows\n' }],
    }),
    desktopFragment({
      version,
      repo: REPO,
      tag,
      payloads: [
        { platform: 'linux-x86_64', name: `fairspoken-${version}-linux-x86_64.AppImage`, signature: 'sig-appimage' },
        { platform: 'linux-x86_64-deb', name: `fairspoken-${version}-linux-x86_64.deb`, signature: 'sig-deb' },
      ],
    }),
  ]
  return mergeFragments(fragments, { notes: 'n', pub_date: '2026-09-11T00:00:00.000Z' })
}

test('the merged desktop manifest carries every platform, the deb included, and passes the check', () => {
  const manifest = desktopManifest()
  assert.equal(manifest.version, '0.4.0')
  assert.deepEqual(Object.keys(manifest.platforms), ['windows-x86_64', 'linux-x86_64', 'linux-x86_64-deb'])
  assert.equal(manifest.platforms['windows-x86_64'].signature, 'sig-windows')
  assert.equal(manifest.platforms['linux-x86_64-deb'].url, `https://github.com/${REPO}/releases/download/v0.4.0/fairspoken-0.4.0-linux-x86_64.deb`)
  assert.deepEqual(checkDesktopManifest(JSON.stringify(manifest), { version: '0.4.0', repo: REPO, tag: TAG, assetNames: RELEASE_ASSETS }), [])
})

test('merging refuses fragments that disagree or overlap', () => {
  const { platforms } = desktopManifest()
  const windows = { version: '0.4.0', platforms: { 'windows-x86_64': platforms['windows-x86_64'] } }
  const linux = { version: '0.4.1', platforms: { 'linux-x86_64': platforms['linux-x86_64'] } }
  assert.throws(() => mergeFragments([windows, linux]), /disagree on version/)
  assert.throws(() => mergeFragments([windows, windows]), /both claim windows-x86_64/)
  assert.throws(() => mergeFragments([]), /No manifest fragments/)
  assert.throws(
    () => desktopFragment({ version: '0.4.0', repo: REPO, tag: TAG, payloads: [{ platform: 'linux-x86_64', name: 'x', signature: ' \n' }] }),
    /signature is empty/,
  )
})

test('checkDesktopManifest names what a partial or misdirected manifest lacks', () => {
  const check = (doc, overrides = {}) =>
    checkDesktopManifest(JSON.stringify(doc), { version: '0.4.0', repo: REPO, tag: TAG, assetNames: RELEASE_ASSETS, ...overrides })
  const noDeb = desktopManifest()
  delete noDeb.platforms['linux-x86_64-deb']
  assert.deepEqual(check(noDeb), ['has no linux-x86_64-deb entry, so those installs are never offered it'])
  assert.deepEqual(check(desktopManifest(), { version: '0.4.1' }), ['names version 0.4.0, expected 0.4.1'])
  const elsewhere = JSON.parse(JSON.stringify(desktopManifest()).replaceAll(`github.com/${REPO}/`, 'github.com/acme/other/'))
  assert.equal(check(elsewhere).length, 3)
  assert.match(check(elsewhere)[0], /not at v0\.4\.0 on acme\/fairspoken/)
  assert.deepEqual(check(desktopManifest(), { assetNames: RELEASE_ASSETS.filter((name) => !name.endsWith('.deb')) }), [
    'points linux-x86_64-deb at fairspoken-0.4.0-linux-x86_64.deb, which is not on the release',
  ])
  const swapped = desktopManifest()
  swapped.platforms['linux-x86_64-deb'].url = swapped.platforms['linux-x86_64'].url
  assert.deepEqual(check(swapped), ['points linux-x86_64-deb at fairspoken-0.4.0-linux-x86_64.AppImage, not a *.deb'])
  const unsigned = desktopManifest()
  unsigned.platforms['windows-x86_64'].signature = ''
  assert.deepEqual(check(unsigned), ['has no windows-x86_64 signature, and the updater refuses an unsigned payload'])
  assert.match(checkDesktopManifest('version: 0.4.0', { version: '0.4.0', repo: REPO, tag: TAG, assetNames: [] })[0], /does not parse/)
})

const SHA256 = 'c'.repeat(64)

function hostManifest(version = '0.4.0', tag = TAG) {
  const fragments = ['linux-x64', 'linux-arm64', 'windows-x64', 'macos-arm64'].map((target) =>
    hostFragment({ slug: SLUG, version, repo: REPO, tag, target, sha256: SHA256, signature: `sig-${target}\n` }),
  )
  return mergeFragments(fragments, { notes: 'n', pub_date: '2026-09-11T00:00:00.000Z' })
}

test('the host manifest is keyed by the contract platforms, with url, sha256, signature and format', () => {
  const manifest = hostManifest()
  assert.deepEqual(Object.keys(manifest.platforms).sort(), [...HOST_PLATFORMS].sort())
  assert.deepEqual(HOST_PLATFORMS, ['linux-x86_64', 'linux-aarch64', 'windows-x86_64', 'darwin-aarch64'])
  assert.deepEqual(manifest.platforms['windows-x86_64'], {
    url: `https://github.com/${REPO}/releases/download/v0.4.0/fairspoken-0.4.0-transcription-host-windows-x64.zip`,
    sha256: SHA256,
    signature: 'sig-windows-x64',
    format: 'zip',
  })
  assert.equal(manifest.platforms['darwin-aarch64'].format, 'tar.gz')
  assert.deepEqual(checkHostManifest(manifest, { version: '0.4.0', repo: REPO, tag: TAG, assetNames: RELEASE_ASSETS }), [])
  assert.throws(() => hostFragment({ slug: SLUG, version: '0.4.0', repo: REPO, tag: TAG, target: 'linux-x64', sha256: 'abc', signature: 's' }), /Not a sha256/)
  assert.throws(() => hostFragment({ slug: SLUG, version: '0.4.0', repo: REPO, tag: TAG, target: 'linux-x64', sha256: SHA256, signature: '' }), /empty/)
})

test('checkHostManifest names a missing platform, a bad hash, a wrong format and a missing archive', () => {
  const check = (doc, assetNames = RELEASE_ASSETS) => checkHostManifest(doc, { version: '0.4.0', repo: REPO, tag: TAG, assetNames })
  const broken = hostManifest()
  delete broken.platforms['linux-aarch64']
  broken.platforms['darwin-aarch64'].sha256 = 'nope'
  broken.platforms['windows-x86_64'].format = 'tar.gz'
  assert.deepEqual(check(broken), ['has no linux-aarch64 entry', 'says windows-x86_64 is tar.gz, not zip', 'has no sha256 for darwin-aarch64'])
  assert.deepEqual(check(hostManifest(), RELEASE_ASSETS.filter((name) => !name.includes('macos-arm64.tar.gz'))), [
    'points darwin-aarch64 at fairspoken-0.4.0-transcription-host-macos-arm64.tar.gz, which is not on the release',
  ])
})

test('the host archive layout names the one folder and the binary inside it', () => {
  assert.deepEqual(hostArchiveLayout({ slug: SLUG, version: '0.5.0', target: 'windows-x64' }), {
    archive: 'fairspoken-0.5.0-transcription-host-windows-x64.zip',
    folder: 'fairspoken-0.5.0-transcription-host-windows-x64',
    binary: 'fairspoken-0.5.0-transcription-host-windows-x64/transcription-host.exe',
    platform: 'windows-x86_64',
    format: 'zip',
  })
  assert.equal(
    hostArchiveLayout({ slug: SLUG, version: '0.5.0-nightly.20261004.41', target: 'macos-arm64' }).binary,
    'fairspoken-0.5.0-nightly.20261004.41-transcription-host-macos-arm64/transcription-host',
  )
})

const ZIP = 'Fairspoken-0.4.0-macos-arm64.zip'
const sparkleMeta = (overrides = {}) => ({
  bundleId: 'ie.fairspoken.mac',
  version: '202610051230',
  shortVersionString: '0.4.0',
  minimumSystemVersion: '26.0',
  file: ZIP,
  length: 1234,
  edSignature: Buffer.alloc(64, 7).toString('base64'),
  publicEDKey: 'SPGwzOHQk1H5lvqePdjg11xTC8LT3/AX3kILlbOIxp0=',
  ...overrides,
})

test('Sparkle metadata must describe a notarized zip on the release, for the right app and build', () => {
  const [client, server] = SPARKLE_APPS
  assert.equal(client.appcast, 'appcast-fairspoken.xml')
  assert.equal(server.appcast, 'appcast-fairspoken-server.xml')
  const assets = [{ name: ZIP, size: 1234 }, { name: 'Fairspoken-0.4.0-macos-arm64-unsigned.zip', size: 9 }]
  const check = (meta, app = client) => checkSparkleMetadata(JSON.stringify(meta), { app, version: '0.4.0', assets })
  assert.deepEqual(check(sparkleMeta()), [])
  assert.deepEqual(check(sparkleMeta(), server), ['is for ie.fairspoken.mac, not ie.fairspoken.server'])
  assert.deepEqual(check(sparkleMeta({ version: '41' })), ['has build 41, not a YYYYMMDDHHMM build number'])
  assert.deepEqual(check(sparkleMeta({ length: 99 })), [`says ${ZIP} is 99 bytes, the release has 1234`])
  assert.deepEqual(check(sparkleMeta({ edSignature: 'abc' })), ['has no 64-byte edSignature'])
  assert.deepEqual(check(sparkleMeta({ file: 'Fairspoken-0.4.0-macos-arm64-unsigned.zip', length: 9 })), [
    'names Fairspoken-0.4.0-macos-arm64-unsigned.zip, not a notarized .zip',
  ])
  assert.deepEqual(check(sparkleMeta({ shortVersionString: '0.3.9' })), ['names version 0.3.9, expected 0.4.0'])
  assert.deepEqual(check(sparkleMeta({ minimumSystemVersion: '' })), ['has no minimumSystemVersion'])
})

test('missingInstallers wants every product and the metadata the feeds are rendered from', () => {
  const options = { slug: SLUG, version: '0.4.0', macAppName: 'Fairspoken' }
  assert.deepEqual(missingInstallers(RELEASE_ASSETS, options), [])
  assert.deepEqual(missingInstallers([], options), [
    'the desktop NSIS installer (fairspoken-0.4.0-windows-x64-setup.exe)',
    'the desktop AppImage (fairspoken-0.4.0-linux-x86_64.AppImage)',
    'the desktop deb (fairspoken-0.4.0-linux-x86_64.deb)',
    'the desktop updater manifest (desktop-manifest.json)',
    'the linux-x64 transcription host (fairspoken-0.4.0-transcription-host-linux-x64.tar.gz)',
    'the linux-arm64 transcription host (fairspoken-0.4.0-transcription-host-linux-arm64.tar.gz)',
    'the windows-x64 transcription host (fairspoken-0.4.0-transcription-host-windows-x64.zip)',
    'the macos-arm64 transcription host (fairspoken-0.4.0-transcription-host-macos-arm64.tar.gz)',
    'the transcription host manifest (host-manifest.json)',
    'the native macOS app .zip (Fairspoken-0.4.0-macos-arm64.zip)',
    'the native macOS app .dmg (Fairspoken-0.4.0-macos-arm64.dmg)',
    'the native macOS server app .zip (Fairspoken-Server-0.4.0-macos-arm64.zip)',
    'the native macOS server app .dmg (Fairspoken-Server-0.4.0-macos-arm64.dmg)',
  ])
  // A commit from before apps/macos existed ships no native app.
  assert.equal(missingInstallers(RELEASE_ASSETS, { ...options, macAppName: null }).length, 0)
  assert.equal(missingInstallers([], { ...options, macAppName: null }).length, 9)
  // No latest.json or nightly.json is asked for any more.
  assert.ok(!missingInstallers([], options).some((line) => /latest\.json|nightly\.json/.test(line)))
})

test('the Tauri macOS app is no longer asked for, and its files do not stand in for anything', () => {
  const options = { slug: SLUG, version: '0.4.0', macAppName: 'Fairspoken' }
  assert.ok(!RELEASE_ASSETS.some((name) => /^fairspoken-.*-macos-(arm64|x64)(\.dmg|\.app\.tar\.gz)$/.test(name)))
  for (const target of ['macos-arm64']) {
    const name = `fairspoken-0.4.0-transcription-host-${target}.tar.gz`
    assert.deepEqual(missingInstallers(RELEASE_ASSETS.filter((asset) => asset !== name), options), [`the ${target} transcription host (${name})`])
  }
  const tauriDmgOnly = [...RELEASE_ASSETS.filter((name) => name !== 'Fairspoken-0.4.0-macos-arm64.dmg'), 'fairspoken-0.4.0-macos-arm64.dmg']
  assert.deepEqual(missingInstallers(tauriDmgOnly, options), ['the native macOS app .dmg (Fairspoken-0.4.0-macos-arm64.dmg)'])
})

test("the client's and the server's packages never stand in for each other", () => {
  const options = { slug: SLUG, version: '0.4.0', macAppName: 'Fairspoken' }
  const withoutClient = RELEASE_ASSETS.filter((name) => !name.startsWith('Fairspoken-0.4.0-'))
  assert.deepEqual(missingInstallers(withoutClient, options), [
    'the native macOS app .zip (Fairspoken-0.4.0-macos-arm64.zip)',
    'the native macOS app .dmg (Fairspoken-0.4.0-macos-arm64.dmg)',
  ])
  const serverDmgOnly = RELEASE_ASSETS.filter((name) => name !== 'Fairspoken-Server-0.4.0-macos-arm64.dmg')
  assert.deepEqual(missingInstallers(serverDmgOnly, options), ['the native macOS server app .dmg (Fairspoken-Server-0.4.0-macos-arm64.dmg)'])
})

test('ad-hoc signed native apps complete a release, are named as such, and are not missing Sparkle metadata', () => {
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
  assert.deepEqual(macAppsWithoutSparkle(unsigned, options), [])
  assert.deepEqual(macAppsWithoutSparkle(RELEASE_ASSETS, options), ['Fairspoken', 'Fairspoken Server'])
  assert.deepEqual(macAppsWithoutSparkle([...RELEASE_ASSETS, 'sparkle-fairspoken.json'], options), ['Fairspoken Server'])
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
  assert.equal(macAppFileName({ appName: 'Fairspoken', version: '0.5.0', signed: false, extension: '.zip' }), 'Fairspoken-0.5.0-macos-arm64-unsigned.zip')
  assert.throws(() => macAppFileName({ appName: 'Fair spoken', version: '0.5.0', extension: '.zip' }), /Cannot name files/)
  assert.throws(() => macAppFileName({ appName: 'Fairspoken', version: '0.5.0', extension: '.pkg' }), /Unknown macOS app package/)
  assert.equal(macServerName('Fairspoken'), 'Fairspoken-Server')
  assert.equal(macServerName(null), null)
})

test("the native app's name is read from MV_DISPLAY_NAME in its xcconfig", () => {
  const xcconfig = '// PRODUCT NAME\nMV_DISPLAY_NAME = Fairspoken\nMV_BUNDLE_ID_BASE = ie.fairspoken.mac\n'
  assert.equal(macAppNameFromXcconfig(xcconfig), 'Fairspoken')
  assert.equal(macAppNameFromXcconfig('MARKETING_VERSION = 0.1.0\n'), null)
  assert.equal(macAppNameFromXcconfig(undefined), null)
})

const allSecrets = () =>
  Object.fromEntries(
    [
      'TAURI_SIGNING_PRIVATE_KEY',
      'CSC_LINK',
      'CSC_KEY_PASSWORD',
      'APPLE_ID',
      'APPLE_APP_SPECIFIC_PASSWORD',
      'APPLE_TEAM_ID',
      'AZURE_TENANT_ID',
      'AZURE_CLIENT_ID',
      'AZURE_CLIENT_SECRET',
      'AZURE_TRUSTED_SIGNING_ENDPOINT',
      'AZURE_TRUSTED_SIGNING_ACCOUNT_NAME',
      'AZURE_TRUSTED_SIGNING_CERTIFICATE_PROFILE_NAME',
      'SPARKLE_ED_PRIVATE_KEY',
    ].map((name) => [name, true]),
  )

test('secrets: everything set signs everything and feeds the appcasts', () => {
  assert.deepEqual(checkSecrets(allSecrets()), { errors: [], warnings: [], macSigning: true, sparkle: true, windowsSigning: true })
})

test('secrets: the Tauri key is required, and a partial group fails before anything is built', () => {
  const noTauri = { ...allSecrets(), TAURI_SIGNING_PRIVATE_KEY: false }
  assert.match(checkSecrets(noTauri).errors[0], /TAURI_SIGNING_PRIVATE_KEY must be set/)
  const partialApple = { ...allSecrets(), APPLE_TEAM_ID: false }
  const result = checkSecrets(partialApple)
  assert.equal(result.errors.length, 1)
  assert.match(result.errors[0], /Only 4 of the 5 apple secrets are set \(missing APPLE_TEAM_ID\)/)
  assert.equal(result.macSigning, false)
  assert.equal(result.sparkle, false)
})

test('secrets: an empty optional group warns and ships that platform unsigned or unfed', () => {
  const none = { TAURI_SIGNING_PRIVATE_KEY: true }
  const result = checkSecrets(none)
  assert.deepEqual(result.errors, [])
  assert.equal(result.warnings.length, 3)
  assert.equal(result.macSigning, false)
  assert.equal(result.windowsSigning, false)
  const noSparkle = checkSecrets({ ...allSecrets(), SPARKLE_ED_PRIVATE_KEY: false })
  assert.deepEqual(noSparkle.errors, [])
  assert.equal(noSparkle.sparkle, false)
  assert.match(noSparkle.warnings[0], /No sparkle secrets: the native macOS apps get no appcast entry/)
  // A Sparkle key without the Apple secrets has nothing to sign.
  const sparkleOnly = checkSecrets({ TAURI_SIGNING_PRIVATE_KEY: true, SPARKLE_ED_PRIVATE_KEY: true })
  assert.equal(sparkleOnly.sparkle, false)
  assert.ok(sparkleOnly.warnings.some((warning) => /SPARKLE_ED_PRIVATE_KEY is set but the Apple secrets are not/.test(warning)))
})

test('anonymousGet sends no credential, whatever is in the environment', async () => {
  const sent = []
  const previousToken = process.env.GH_TOKEN
  process.env.GH_TOKEN = 'ghp_not_a_real_token'
  try {
    const get = anonymousGet(async (url, init) => {
      sent.push({ url, headers: init.headers })
      return { ok: true, status: 200, text: async () => 'body' }
    })
    assert.deepEqual(await get(`https://github.com/${REPO}/releases/download/update-feeds/desktop-stable.json`), {
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
  const response = await get(`https://github.com/${REPO}/releases`)
  assert.equal(response.ok, false)
  assert.equal(response.status, 0)
  assert.match(response.error, /ENOTFOUND/)
})

test('only nightlies past the newest few are pruned, never a stable, a draft, update-feeds or a stranger', () => {
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
    release('update-feeds', '01', { prerelease: true }),
    release('some-other-tag', '01'),
  ]
  assert.equal(NIGHTLIES_KEPT, 3)
  assert.deepEqual(nightliesToPrune(releases), ['v0.7.0-nightly.20260924.31', 'v0.7.0-nightly.20260923.28'])
  assert.deepEqual(nightliesToPrune(releases, 5), [])
  assert.deepEqual(nightliesToPrune(releases, 1).includes('update-feeds'), false)
  assert.deepEqual(nightliesToPrune([]), [])
  assert.throws(() => nightliesToPrune(releases, 0), /Keep at least one nightly/)
})
