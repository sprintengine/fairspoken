// node --test scripts/release/feeds.test.mjs  (npm run test:release)

import assert from 'node:assert/strict'
import { test } from 'node:test'

import {
  APPCAST_ITEMS_PER_CHANNEL,
  appcastItems,
  checkPublicFeeds,
  FEED_NAMES,
  feedsCoverRelease,
  feedUrl,
  planFeeds,
  renderFeeds,
  verifyPublicFeeds,
} from './feeds.mjs'
import { FIXTURE_REPO, fixtureRelease, fixtureReleases } from './fixtures/releases.mjs'
import { DESKTOP_MANIFEST, SPARKLE_APPS } from './release-lib.mjs'

const REPO = FIXTURE_REPO
const read = async (release, name) => release.assets.find((asset) => asset.name === name).content
const render = (releases) => renderFeeds(planFeeds(releases), { repo: REPO, read })
const tags = (list) => list.map((release) => release.tag_name)

test('the feed names and URLs are the contract', () => {
  assert.deepEqual(FEED_NAMES, [
    'desktop-stable.json',
    'desktop-nightly.json',
    'appcast-fairspoken.xml',
    'appcast-fairspoken-server.xml',
    'host-stable.json',
    'host-nightly.json',
  ])
  assert.equal(
    feedUrl({ repo: REPO, name: 'appcast-fairspoken.xml' }),
    'https://github.com/sprintengine/fairspoken/releases/download/update-feeds/appcast-fairspoken.xml',
  )
})

test('planFeeds: stable by version, nightly by publish time, never a draft, a stranger or a nightly about to be pruned', () => {
  const plan = planFeeds(fixtureReleases())
  assert.deepEqual(plan.pruned, ['v0.4.0-nightly.20260930.40', 'v0.4.0-nightly.20260928.31'])
  assert.deepEqual(tags(plan.desktop.stable), ['v0.4.0', 'v0.3.0', 'v0.2.1', 'v0.2.0'])
  assert.deepEqual(tags(plan.desktop.nightly), ['v0.4.1-nightly.20261004.71', 'v0.4.1-nightly.20261003.60', 'v0.4.1-nightly.20261002.52'])
  // The -unsigned nightly has no Sparkle metadata, so it is no appcast candidate.
  assert.deepEqual(tags(plan.appcasts[0].channels.nightly), ['v0.4.1-nightly.20261004.71', 'v0.4.1-nightly.20261002.52'])
  for (const list of [plan.desktop.stable, plan.desktop.nightly, plan.host.stable, plan.host.nightly]) {
    assert.ok(!tags(list).some((tag) => tag === 'v0.5.0' || tag === 'update-feeds' || tag === 'some-other-tag'))
  }
})

test('desktop feeds: newest stable and newest nightly, with all three platforms and the deb', async () => {
  const { files, warnings } = await render(fixtureReleases())
  assert.deepEqual(warnings, [])
  const stable = JSON.parse(files['desktop-stable.json'])
  assert.deepEqual(Object.keys(stable), ['version', 'notes', 'pub_date', 'platforms'])
  assert.equal(stable.version, '0.4.0')
  assert.equal(stable.notes, 'https://github.com/sprintengine/fairspoken/releases/tag/v0.4.0')
  assert.equal(stable.pub_date, '2026-10-01T09:00:00.000Z')
  assert.deepEqual(Object.keys(stable.platforms), ['windows-x86_64', 'linux-x86_64', 'linux-x86_64-deb'])
  assert.equal(
    stable.platforms['linux-x86_64-deb'].url,
    'https://github.com/sprintengine/fairspoken/releases/download/v0.4.0/fairspoken-0.4.0-linux-x86_64.deb',
  )
  assert.ok(stable.platforms['linux-x86_64-deb'].signature.length > 40)
  const nightly = JSON.parse(files['desktop-nightly.json'])
  assert.equal(nightly.version, '0.4.1-nightly.20261004.71')
  // Every url points at the real release, not at update-feeds.
  for (const entry of Object.values(nightly.platforms)) assert.match(entry.url, /\/releases\/download\/v0\.4\.1-nightly\.20261004\.71\//)
})

test('host feeds: the contract shape, the five platforms, and the channel spelled out', async () => {
  const { files } = await render(fixtureReleases())
  const host = JSON.parse(files['host-nightly.json'])
  assert.deepEqual(Object.keys(host), ['version', 'channel', 'pub_date', 'notes', 'platforms'])
  assert.equal(host.version, '0.4.1-nightly.20261004.71')
  assert.equal(host.channel, 'nightly')
  assert.equal(host.notes, 'https://github.com/sprintengine/fairspoken/releases/tag/v0.4.1-nightly.20261004.71')
  assert.deepEqual(Object.keys(host.platforms), ['linux-x86_64', 'linux-aarch64', 'windows-x86_64', 'darwin-aarch64', 'darwin-x86_64'])
  for (const [platform, entry] of Object.entries(host.platforms)) {
    assert.deepEqual(Object.keys(entry), ['url', 'sha256', 'signature', 'format'])
    assert.match(entry.sha256, /^[0-9a-f]{64}$/)
    assert.equal(entry.format, platform === 'windows-x86_64' ? 'zip' : 'tar.gz')
  }
  assert.equal(JSON.parse(files['host-stable.json']).channel, 'stable')
})

test('appcasts: the newest three stable and the kept nightlies, newest build first, nightly items tagged', async () => {
  const { files, sources } = await render(fixtureReleases())
  for (const app of SPARKLE_APPS) {
    const items = appcastItems(files[app.appcast])
    assert.deepEqual(
      items.map((item) => [item.shortVersionString, item.channel]),
      [
        ['0.4.1-nightly.20261004.71', 'nightly'],
        ['0.4.1-nightly.20261002.52', 'nightly'],
        ['0.4.0', null],
        ['0.3.0', null],
        ['0.2.1', null],
      ],
    )
    // Builds strictly descending: the stable promoted from a nightly is above it.
    const builds = items.map((item) => Number(item.version))
    assert.deepEqual([...builds].sort((a, b) => b - a), builds)
    for (const item of items) {
      assert.match(item.url, new RegExp(`/releases/download/v${item.shortVersionString.replaceAll('.', '\\.')}/`))
      assert.ok(item.url.endsWith('-macos-arm64.zip'))
      assert.equal(item.minimumSystemVersion, '26.0')
      assert.equal(Buffer.from(item.edSignature, 'base64').length, 64)
      assert.ok(item.length > 0)
    }
    assert.deepEqual(sources[app.appcast].length, 5)
  }
  const server = appcastItems(files['appcast-fairspoken-server.xml'])
  assert.ok(server.every((item) => item.url.includes('/Fairspoken-Server-')))
  const xml = files['appcast-fairspoken.xml']
  assert.match(xml, /^<\?xml version="1\.0" encoding="utf-8"\?>\n<rss version="2\.0" xmlns:sparkle="http:\/\/www\.andymatuschak\.org\/xml-namespaces\/sparkle"/)
  assert.match(xml, /<sparkle:releaseNotesLink>https:\/\/github\.com\/sprintengine\/fairspoken\/releases\/tag\/v0\.4\.0<\/sparkle:releaseNotesLink>/)
  assert.match(xml, /<pubDate>Thu, 01 Oct 2026 09:00:00 GMT<\/pubDate>/)
  // Stable items carry no channel element at all.
  const stableItem = xml.split('<item>').find((chunk) => chunk.includes('<sparkle:shortVersionString>0.4.0<'))
  assert.ok(!stableItem.includes('sparkle:channel'))
})

test('appcasts keep at most three per channel, and a pruned nightly drops out', async () => {
  const many = Array.from({ length: 6 }, (_, i) =>
    fixtureRelease({ version: `0.9.0-nightly.202610${10 + i}.${i + 1}`, publishedAt: `2026-10-${10 + i}T12:00:00Z`, build: `202610${10 + i}1200` }),
  )
  const { files } = await render([...many, ...fixtureReleases()])
  const nightlies = appcastItems(files['appcast-fairspoken.xml']).filter((item) => item.channel === 'nightly')
  assert.equal(nightlies.length, APPCAST_ITEMS_PER_CHANNEL)
  assert.deepEqual(
    nightlies.map((item) => item.shortVersionString),
    ['0.9.0-nightly.20261015.6', '0.9.0-nightly.20261014.5', '0.9.0-nightly.20261013.4'],
  )
  // Once the newer nightlies push 20261004.71 past retention, it is gone from
  // every feed, not just behind the newer ones.
  assert.ok(!Object.values(files).some((text) => text.includes('20261004.71')))
})

test('a release whose metadata does not check out is skipped, never published', async () => {
  const releases = fixtureReleases()
  const newest = releases.find((release) => release.tag_name === 'v0.4.1-nightly.20261004.71')
  const manifest = newest.assets.find((asset) => asset.name === DESKTOP_MANIFEST)
  const doc = JSON.parse(manifest.content)
  delete doc.platforms['linux-x86_64-deb']
  manifest.content = JSON.stringify(doc)
  const meta = newest.assets.find((asset) => asset.name === 'sparkle-fairspoken.json')
  meta.content = '{not json'
  const { files, warnings } = await render(releases)
  assert.equal(JSON.parse(files['desktop-nightly.json']).version, '0.4.1-nightly.20261003.60')
  assert.equal(JSON.parse(files['host-nightly.json']).version, '0.4.1-nightly.20261004.71')
  assert.ok(!appcastItems(files['appcast-fairspoken.xml']).some((item) => item.shortVersionString === '0.4.1-nightly.20261004.71'))
  assert.ok(appcastItems(files['appcast-fairspoken-server.xml']).some((item) => item.shortVersionString === '0.4.1-nightly.20261004.71'))
  assert.equal(warnings.length, 2)
  assert.match(warnings[0], /desktop-manifest\.json is not usable, skipped: has no linux-x86_64-deb entry/)
  assert.match(warnings[1], /sparkle-fairspoken\.json is not usable, skipped: cannot be read/)
  // ...and the publish that made it would then fail its own check.
  assert.deepEqual(
    feedsCoverRelease(files, { version: '0.4.1-nightly.20261004.71', channel: 'nightly', sparkleApps: SPARKLE_APPS }),
    [
      'desktop-nightly.json names 0.4.1-nightly.20261003.60, not 0.4.1-nightly.20261004.71',
      'appcast-fairspoken.xml has no item for 0.4.1-nightly.20261004.71',
    ],
  )
})

test('a new repository with one nightly: no stable JSON feeds yet, empty-but-valid appcast stable channels', async () => {
  const first = fixtureRelease({ version: '0.2.0-nightly.20261005.3', publishedAt: '2026-10-05T12:00:00Z', build: '202610051200' })
  const { files } = await render([first])
  assert.deepEqual(Object.keys(files).sort(), ['appcast-fairspoken-server.xml', 'appcast-fairspoken.xml', 'desktop-nightly.json', 'host-nightly.json'])
  assert.deepEqual(feedsCoverRelease(files, { version: '0.2.0-nightly.20261005.3', channel: 'nightly', sparkleApps: SPARKLE_APPS }), [])
  const { files: none } = await render([])
  assert.deepEqual(appcastItems(none['appcast-fairspoken.xml']), [])
  assert.match(none['appcast-fairspoken.xml'], /<channel>[\s\S]*<\/channel>/)
})

test('feedsCoverRelease: the stable just published must head the stable feeds', async () => {
  const { files } = await render(fixtureReleases())
  assert.deepEqual(feedsCoverRelease(files, { version: '0.4.0', channel: 'stable', sparkleApps: SPARKLE_APPS }), [])
  assert.deepEqual(feedsCoverRelease(files, { version: '0.4.2', channel: 'stable' }), [
    'desktop-stable.json names 0.4.0, not 0.4.2',
    'host-stable.json names 0.4.0, not 0.4.2',
  ])
})

// The unauthenticated read-back. Every request is served from a fixture: these
// tests describe what an installed app would see.

const ok = (text) => ({ ok: true, status: 200, text })
const servedBy = (responses) => async (url) => responses[url] ?? { ok: false, status: 404, text: '' }

async function world({ channel = 'stable', tag = 'v0.4.0', latest = 'v0.4.0' } = {}) {
  const { files } = await render(fixtureReleases())
  const responses = {
    [`https://github.com/${REPO}/releases/tag/update-feeds`]: ok('<html>'),
    [`https://github.com/${REPO}/releases/latest`]: ok(JSON.stringify({ tag_name: latest })),
  }
  for (const [name, text] of Object.entries(files)) responses[feedUrl({ repo: REPO, name })] = ok(text)
  return { files, responses, release: { tag, channel } }
}

test('public feeds that match what was uploaded have nothing to report', async () => {
  const stable = await world()
  assert.deepEqual((await checkPublicFeeds({ repo: REPO, ...stable, get: servedBy(stable.responses) })).problems, [])
  const nightly = await world({ channel: 'nightly', tag: 'v0.4.1-nightly.20261004.71' })
  assert.deepEqual((await checkPublicFeeds({ repo: REPO, ...nightly, get: servedBy(nightly.responses) })).problems, [])
  // A feeds-only run has no release to check the pointer for.
  assert.deepEqual((await checkPublicFeeds({ repo: REPO, files: stable.files, get: servedBy(stable.responses) })).problems, [])
})

test('a repository no user can read fails at once, and says what to look at', async () => {
  const waits = []
  const { files } = await world()
  const problems = await verifyPublicFeeds({ repo: REPO, files, get: servedBy({}), attempts: 5, delayMs: 1, wait: async (ms) => waits.push(ms) })
  assert.equal(problems.length, 1)
  assert.match(problems[0], /update-feeds answered HTTP 404 without credentials/)
  assert.match(problems[0], /must be public/)
  assert.deepEqual(waits, [])
})

test('a stale or missing feed copy is polled for, not failed on', async () => {
  const w = await world()
  const url = feedUrl({ repo: REPO, name: 'desktop-stable.json' })
  w.responses[url] = ok('{"version":"0.3.0"}')
  delete w.responses[feedUrl({ repo: REPO, name: 'host-stable.json' })]
  const first = await checkPublicFeeds({ repo: REPO, ...w, get: servedBy(w.responses) })
  assert.equal(first.retryable, true)
  assert.equal(first.problems.length, 2)
  const waits = []
  const problems = await verifyPublicFeeds({
    repo: REPO,
    ...w,
    get: servedBy(w.responses),
    attempts: 3,
    delayMs: 1,
    wait: async (ms) => {
      waits.push(ms)
      for (const [name, text] of Object.entries(w.files)) w.responses[feedUrl({ repo: REPO, name })] = ok(text)
    },
  })
  assert.deepEqual(problems, [])
  assert.deepEqual(waits, [1])
})

test('the latest pointer must name a stable just published, and must never name a nightly', async () => {
  const lagging = await world({ latest: 'v0.3.0' })
  const stale = await checkPublicFeeds({ repo: REPO, ...lagging, get: servedBy(lagging.responses) })
  assert.equal(stale.retryable, true)
  assert.match(stale.problems[0], /resolves to v0\.3\.0, not v0\.4\.0/)
  const tag = 'v0.4.1-nightly.20261004.71'
  const mislabelled = await world({ channel: 'nightly', tag, latest: tag })
  const result = await checkPublicFeeds({ repo: REPO, ...mislabelled, get: servedBy(mislabelled.responses) })
  assert.equal(result.retryable, false)
  assert.match(result.problems[0], /a nightly\. Publish nightlies with --latest=false/)
})
