// Release data shaped like GitHub's list-releases answer, with each metadata
// asset's text inline as `content`. feeds.test.mjs and
// `release.mjs feeds-dry-run` render the update feeds from it.
//
// What it holds, and what the feeds should make of it:
//
//   stable   v0.2.0 v0.2.1 v0.3.0 v0.4.0     the appcasts keep the newest three
//   nightly  ...20260928.31 ...20260930.40    pruned: past the newest three nightlies
//            ...20261002.52 ...20261004.71    kept, with Sparkle metadata
//            ...20261003.60                   kept, but its Mac apps are -unsigned,
//                                             so it has no appcast item
//   v0.5.0 (a draft), update-feeds, and a stranger's tag: ignored

import { createHash } from 'node:crypto'

import {
  DESKTOP_LEGS,
  DESKTOP_MANIFEST,
  desktopFragment,
  HOST_MANIFEST,
  HOST_TARGETS,
  hostArchiveLayout,
  hostFragment,
  macAppFileName,
  mergeFragments,
  releaseFileName,
  releasePageUrl,
  SPARKLE_APPS,
} from '../release-lib.mjs'

export const FIXTURE_REPO = 'sprintengine/fairspoken'
export const FIXTURE_SLUG = 'fairspoken'
export const FIXTURE_MAC_NAME = 'Fairspoken'

const digest = (text, length) => createHash('sha512').update(text).digest().subarray(0, length)
// A string shaped like a tauri signer .sig: base64 of a minisign signature file.
const fakeMinisign = (name) =>
  Buffer.from(
    `untrusted comment: signature from tauri secret key\n${digest(`sig ${name}`, 74).toString('base64')}\n` +
      `trusted comment: timestamp:1791200000\tfile:${name}\n${digest(`global ${name}`, 64).toString('base64')}\n`,
  ).toString('base64')

let nextId = 1000
const asset = (name, size, content) => ({ id: nextId++, name, size, ...(content === undefined ? {} : { content }) })

// One complete release: every product, its metadata, and (when `sparkle`)
// notarized Mac apps with their Sparkle metadata.
export function fixtureRelease({ version, publishedAt, build, sparkle = true, repo = FIXTURE_REPO }) {
  const tag = `v${version}`
  const assets = []
  const desktopPayloads = []
  for (const [leg, { payloads }] of Object.entries(DESKTOP_LEGS)) {
    for (const payload of payloads) {
      const name = releaseFileName({ slug: FIXTURE_SLUG, version, platform: leg, suffix: payload.suffix })
      assets.push(asset(name, 90_000_000))
      desktopPayloads.push({ platform: payload.platform, name, signature: fakeMinisign(name) })
    }
  }
  const desktop = mergeFragments([desktopFragment({ version, repo, tag, payloads: desktopPayloads })], {
    notes: releasePageUrl({ repo, tag }),
    pub_date: publishedAt,
  })
  assets.push(asset(DESKTOP_MANIFEST, 2_000, `${JSON.stringify(desktop, null, 2)}\n`))

  const hostFragments = Object.keys(HOST_TARGETS).map((target) => {
    const { archive } = hostArchiveLayout({ slug: FIXTURE_SLUG, version, target })
    assets.push(asset(archive, 40_000_000))
    const sha256 = digest(`sha ${archive}`, 32).toString('hex')
    return hostFragment({ slug: FIXTURE_SLUG, version, repo, tag, target, sha256, signature: fakeMinisign(archive) })
  })
  const host = mergeFragments(hostFragments, { notes: releasePageUrl({ repo, tag }), pub_date: publishedAt })
  assets.push(asset(HOST_MANIFEST, 3_000, `${JSON.stringify(host, null, 2)}\n`))

  for (const app of SPARKLE_APPS) {
    const stem = app.stem(FIXTURE_MAC_NAME)
    const zip = macAppFileName({ appName: stem, version, signed: sparkle, extension: '.zip' })
    const zipSize = 60_000_000 + (nextId % 997)
    assets.push(asset(zip, zipSize), asset(macAppFileName({ appName: stem, version, signed: sparkle, extension: '.dmg' }), 62_000_000))
    if (sparkle) {
      const meta = {
        bundleId: app.bundleId,
        version: build,
        shortVersionString: version,
        minimumSystemVersion: '26.0',
        file: zip,
        length: zipSize,
        edSignature: digest(`ed ${zip}`, 64).toString('base64'),
        publicEDKey: 'SPGwzOHQk1H5lvqePdjg11xTC8LT3/AX3kILlbOIxp0=',
      }
      assets.push(asset(app.metadata, 400, `${JSON.stringify(meta, null, 2)}\n`))
    }
  }

  return {
    id: nextId++,
    tag_name: tag,
    name: `Fairspoken ${version}`,
    draft: false,
    prerelease: version.includes('-'),
    published_at: publishedAt,
    html_url: releasePageUrl({ repo, tag }),
    body: `Fairspoken ${version}.\n\n<!-- source-sha: ${digest(tag, 20).toString('hex')} -->\n`,
    assets,
  }
}

export function fixtureReleases() {
  const r = (version, day, hour, options = {}) =>
    fixtureRelease({
      version,
      publishedAt: `2026-${day}T${String(hour).padStart(2, '0')}:00:00Z`,
      build: `2026${day.replace('-', '')}${String(hour).padStart(2, '0')}00`,
      ...options,
    })
  return [
    r('0.4.1-nightly.20261004.71', '10-04', 12),
    r('0.4.1-nightly.20261003.60', '10-03', 12, { sparkle: false }),
    r('0.4.1-nightly.20261002.52', '10-02', 12),
    r('0.4.0', '10-01', 9),
    r('0.4.0-nightly.20260930.40', '09-30', 12),
    r('0.4.0-nightly.20260928.31', '09-28', 12),
    r('0.3.0', '09-20', 9),
    r('0.2.1', '09-10', 9),
    r('0.2.0', '09-01', 9),
    { ...r('0.5.0', '10-05', 9), draft: true, published_at: null },
    {
      id: 1,
      tag_name: 'update-feeds',
      name: 'Update feeds',
      draft: false,
      prerelease: true,
      published_at: '2026-09-01T09:05:00Z',
      assets: [asset('desktop-stable.json', 1_000, '{}')],
    },
    { id: 2, tag_name: 'some-other-tag', draft: false, prerelease: false, published_at: '2026-09-15T00:00:00Z', assets: [] },
  ]
}
