// The update feeds (contract §2): every installed app reads exactly one file on
// the rolling `update-feeds` release,
//
//   desktop-stable.json, desktop-nightly.json   the Tauri desktop app
//   appcast-fairspoken.xml                      the Fairspoken Mac client (Sparkle)
//   appcast-fairspoken-server.xml               Fairspoken Server (Sparkle)
//   host-stable.json, host-nightly.json         the standalone transcription host
//
// and every feed is RENDERED, on each publish, from the releases that exist at
// that moment and the small metadata assets each carries (desktop-manifest.json,
// host-manifest.json, sparkle-<app>.json). Nothing is appended to a feed, so a
// pruned nightly drops out of the appcasts and no feed can point at a deleted
// asset. Pure: the caller lists the releases and passes a function that reads a
// metadata asset, so feeds.test.mjs and `release.mjs feeds-dry-run` cover all
// of it offline.

import {
  CHANNELS,
  checkDesktopManifest,
  checkHostManifest,
  checkSparkleMetadata,
  compareCore,
  DESKTOP_MANIFEST,
  DESKTOP_PLATFORMS,
  HOST_MANIFEST,
  HOST_PLATFORMS,
  NIGHTLIES_KEPT,
  nightliesToPrune,
  releaseAssetUrl,
  releasePageUrl,
  SPARKLE_APPS,
  tagChannel,
} from './release-lib.mjs'

export const FEEDS_TAG = 'update-feeds'
export const FEEDS_RELEASE_TITLE = 'Update feeds'
export const FEEDS_RELEASE_BODY =
  'Update feeds read by installed Fairspoken apps (desktop, macOS and the transcription host). ' +
  'Rewritten by the release workflow on every publish; not a release to install. ' +
  'See docs/releasing.md.'

// Newest items kept per channel in each appcast (contract §2).
export const APPCAST_ITEMS_PER_CHANNEL = 3

export const desktopFeedName = (channel) => `desktop-${channel}.json`
export const hostFeedName = (channel) => `host-${channel}.json`

export const FEED_NAMES = [
  ...CHANNELS.map(desktopFeedName),
  ...SPARKLE_APPS.map((app) => app.appcast),
  ...CHANNELS.map(hostFeedName),
]

export function feedUrl({ repo, name }) {
  return releaseAssetUrl({ repo, tag: FEEDS_TAG, name })
}

const hasAsset = (release, name) => (release.assets ?? []).some((asset) => asset.name === name)
const versionOf = (release) => release.tag_name.replace(/^v/, '')

// Which releases each feed is rendered from, newest first, before any metadata
// is read. Only published releases of a known channel count, and never a
// nightly the retention step is about to delete: the feeds are uploaded before
// the prune, and must not name what it removes. Stable releases are ordered by
// version, nightlies by publish time (a promotion resets the nightly base, so a
// nightly's version does not order it).
export function planFeeds(releases, { keepNightlies = NIGHTLIES_KEPT } = {}) {
  const pruned = nightliesToPrune(releases, keepNightlies)
  const live = releases.filter(
    (release) => !release.draft && release.published_at && tagChannel(release.tag_name) && !pruned.includes(release.tag_name),
  )
  const byPublished = (a, b) => Date.parse(b.published_at) - Date.parse(a.published_at)
  const ordered = {
    stable: live.filter((r) => tagChannel(r.tag_name) === 'stable').sort((a, b) => compareCore(b.tag_name, a.tag_name) || byPublished(a, b)),
    nightly: live.filter((r) => tagChannel(r.tag_name) === 'nightly').sort(byPublished),
  }
  const candidates = (channel, asset) => ordered[channel].filter((release) => hasAsset(release, asset))
  return {
    pruned,
    desktop: Object.fromEntries(CHANNELS.map((channel) => [channel, candidates(channel, DESKTOP_MANIFEST)])),
    host: Object.fromEntries(CHANNELS.map((channel) => [channel, candidates(channel, HOST_MANIFEST)])),
    appcasts: SPARKLE_APPS.map((app) => ({
      app,
      channels: Object.fromEntries(CHANNELS.map((channel) => [channel, candidates(channel, app.metadata)])),
    })),
  }
}

const assetNames = (release) => (release.assets ?? []).map((asset) => asset.name)

// The metadata asset parsed, or the reason it could not be.
async function readJson(read, release, name) {
  try {
    return { doc: JSON.parse(await read(release, name)) }
  } catch (error) {
    return { problems: [`cannot be read: ${error.message}`] }
  }
}

function desktopFeed(release, manifest, repo) {
  const platforms = {}
  for (const platform of Object.keys(DESKTOP_PLATFORMS)) platforms[platform] = manifest.platforms[platform]
  return {
    version: versionOf(release),
    notes: releasePageUrl({ repo, tag: release.tag_name }),
    pub_date: new Date(release.published_at).toISOString(),
    platforms,
  }
}

function hostFeed(release, manifest, repo, channel) {
  const platforms = {}
  for (const platform of HOST_PLATFORMS) platforms[platform] = manifest.platforms[platform]
  return {
    version: versionOf(release),
    channel,
    pub_date: new Date(release.published_at).toISOString(),
    notes: releasePageUrl({ repo, tag: release.tag_name }),
    platforms,
  }
}

const xmlEscape = (value) =>
  String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&apos;')

// A Sparkle 2 appcast with both channels in one feed: stable items carry no
// channel element (Sparkle's default channel, which every install sees), and
// nightly items `<sparkle:channel>nightly</sparkle:channel>`, which only an
// install that allows the nightly channel sees. Items newest build first.
export function renderAppcast({ app, repo, items }) {
  const sorted = [...items].sort((a, b) => Number(b.meta.version) - Number(a.meta.version) || (a.channel === 'stable' ? -1 : 1))
  const lines = [
    '<?xml version="1.0" encoding="utf-8"?>',
    '<rss version="2.0" xmlns:sparkle="http://www.andymatuschak.org/xml-namespaces/sparkle" xmlns:dc="http://purl.org/dc/elements/1.1/">',
    '  <channel>',
    `    <title>${xmlEscape(app.title)}</title>`,
    `    <link>${xmlEscape(`https://github.com/${repo}/releases`)}</link>`,
    `    <description>${xmlEscape(`${app.title} updates: the newest ${APPCAST_ITEMS_PER_CHANNEL} stable and nightly builds.`)}</description>`,
    '    <language>en</language>',
  ]
  for (const { release, channel, meta } of sorted) {
    const page = releasePageUrl({ repo, tag: release.tag_name })
    lines.push(
      '    <item>',
      `      <title>${xmlEscape(`${app.title} ${meta.shortVersionString}`)}</title>`,
      `      <link>${xmlEscape(page)}</link>`,
      `      <pubDate>${new Date(release.published_at).toUTCString()}</pubDate>`,
      `      <sparkle:version>${xmlEscape(meta.version)}</sparkle:version>`,
      `      <sparkle:shortVersionString>${xmlEscape(meta.shortVersionString)}</sparkle:shortVersionString>`,
      `      <sparkle:minimumSystemVersion>${xmlEscape(meta.minimumSystemVersion)}</sparkle:minimumSystemVersion>`,
      `      <sparkle:releaseNotesLink>${xmlEscape(page)}</sparkle:releaseNotesLink>`,
    )
    if (channel !== 'stable') lines.push(`      <sparkle:channel>${xmlEscape(channel)}</sparkle:channel>`)
    lines.push(
      `      <enclosure url="${xmlEscape(releaseAssetUrl({ repo, tag: release.tag_name, name: meta.file }))}" ` +
        `length="${meta.length}" type="application/octet-stream" sparkle:edSignature="${xmlEscape(meta.edSignature)}"/>`,
      '    </item>',
    )
  }
  lines.push('  </channel>', '</rss>', '')
  return lines.join('\n')
}

// Every feed, as file name -> text. `read(release, assetName)` returns the text
// of a metadata asset. A release whose metadata does not check out is skipped
// with a warning and the next candidate is used, so one bad release can hold a
// feed back but never break it; the publish's own verify step then fails,
// because its release is missing from the feeds. A JSON feed with no candidate
// at all (no stable published yet) is not written: an installed build reading
// it then reports a failed check, rather than parsing a feed that lies.
// An appcast is always written, empty if need be, since Sparkle reads an empty
// feed as "up to date".
//
// `withdrawn` names those JSON feeds with no candidate, so the upload deletes
// the copy an earlier publish left on update-feeds (every release of that
// channel was deleted, say): leaving it would keep serving a release that no
// longer exists. A feed whose candidates were all skipped is NOT withdrawn: a
// bad release holds a feed back, and the copy already there stays.
export async function renderFeeds(plan, { repo, read }) {
  const files = {}
  const warnings = []
  const sources = {}
  const withdrawn = CHANNELS.flatMap((channel) => [
    ...(plan.desktop[channel].length === 0 ? [desktopFeedName(channel)] : []),
    ...(plan.host[channel].length === 0 ? [hostFeedName(channel)] : []),
  ])
  const skip = (release, name, problems) =>
    warnings.push(`${release.tag_name} ${name} is not usable, skipped: ${problems.join('; ')}`)

  for (const channel of CHANNELS) {
    for (const release of plan.desktop[channel]) {
      const { doc: manifest, problems: unreadable } = await readJson(read, release, DESKTOP_MANIFEST)
      const problems =
        unreadable ?? checkDesktopManifest(manifest, { version: versionOf(release), repo, tag: release.tag_name, assetNames: assetNames(release) })
      if (problems.length > 0) {
        skip(release, DESKTOP_MANIFEST, problems)
        continue
      }
      files[desktopFeedName(channel)] = `${JSON.stringify(desktopFeed(release, manifest, repo), null, 2)}\n`
      sources[desktopFeedName(channel)] = [release.tag_name]
      break
    }
    for (const release of plan.host[channel]) {
      const { doc: manifest, problems: unreadable } = await readJson(read, release, HOST_MANIFEST)
      const problems =
        unreadable ?? checkHostManifest(manifest, { version: versionOf(release), repo, tag: release.tag_name, assetNames: assetNames(release) })
      if (problems.length > 0) {
        skip(release, HOST_MANIFEST, problems)
        continue
      }
      files[hostFeedName(channel)] = `${JSON.stringify(hostFeed(release, manifest, repo, channel), null, 2)}\n`
      sources[hostFeedName(channel)] = [release.tag_name]
      break
    }
  }

  for (const { app, channels } of plan.appcasts) {
    const items = []
    for (const channel of CHANNELS) {
      let taken = 0
      for (const release of channels[channel]) {
        if (taken === APPCAST_ITEMS_PER_CHANNEL) break
        const { doc: meta, problems: unreadable } = await readJson(read, release, app.metadata)
        const problems = unreadable ?? checkSparkleMetadata(meta, { app, version: versionOf(release), assets: release.assets ?? [] })
        if (problems.length > 0) {
          skip(release, app.metadata, problems)
          continue
        }
        items.push({ release, channel, meta })
        taken += 1
      }
    }
    files[app.appcast] = renderAppcast({ app, repo, items })
    sources[app.appcast] = items.map((item) => item.release.tag_name)
  }
  return { files, warnings, sources, withdrawn }
}

// ---------------------------------------------------------------------------
// Reading the feeds back.

// The items of an appcast this module rendered: enough for the checks below
// and the tests, not a general XML parser.
export function appcastItems(xml) {
  return [...String(xml).matchAll(/<item>([\s\S]*?)<\/item>/g)].map(([, body]) => {
    const tag = (name) => new RegExp(`<sparkle:${name}>([^<]*)</sparkle:${name}>`).exec(body)?.[1] ?? null
    const enclosure = /<enclosure ([^>]*)\/>/.exec(body)?.[1] ?? ''
    const attribute = (name) => new RegExp(`${name}="([^"]*)"`).exec(enclosure)?.[1] ?? null
    return {
      version: tag('version'),
      shortVersionString: tag('shortVersionString'),
      minimumSystemVersion: tag('minimumSystemVersion'),
      channel: tag('channel'),
      url: attribute('url')?.replaceAll('&amp;', '&') ?? null,
      length: Number(attribute('length')),
      edSignature: attribute('sparkle:edSignature'),
    }
  })
}

// Does the freshly rendered set of feeds carry the release just published?
// `sparkleApps` is the SPARKLE_APPS whose metadata the release carries.
export function feedsCoverRelease(files, { version, channel, sparkleApps = [] }) {
  const problems = []
  for (const name of [desktopFeedName(channel), hostFeedName(channel)]) {
    const text = files[name]
    if (!text) {
      problems.push(`${name} was not written`)
      continue
    }
    const served = JSON.parse(text).version
    if (served !== version) problems.push(`${name} names ${served}, not ${version}`)
  }
  for (const app of sparkleApps) {
    const items = appcastItems(files[app.appcast] ?? '')
    if (!items.some((item) => item.shortVersionString === version)) problems.push(`${app.appcast} has no item for ${version}`)
  }
  return problems
}

const describeResponse = (response) => (response.error ? `failed: ${response.error}` : `HTTP ${response.status}`)

// One unauthenticated pass over the feeds just uploaded and, for a release
// run, the release itself. Returns the problems and whether waiting could still
// fix them: release downloads are served through a cache that can lag an upload
// by seconds, while a private repository or a nightly marked latest answers the
// same way forever and polling those only delays the failure.
export async function checkPublicFeeds({ repo, files, release = null, get }) {
  const pending = []
  const settled = []
  const result = () => ({ problems: [...pending, ...settled], retryable: settled.length === 0 && pending.length > 0 })

  const page = `https://github.com/${repo}/releases/tag/${FEEDS_TAG}`
  const visible = await get(page, { accept: 'text/html' })
  if (!visible.ok && visible.status !== 0) {
    settled.push(
      `${page} answered ${describeResponse(visible)} without credentials. Installed apps read their updates ` +
        `from that release, so ${repo} must be public, and the updater endpoint in tauri.conf.json must name it.`,
    )
    return result()
  }
  if (!visible.ok) pending.push(`${page} ${describeResponse(visible)}`)

  for (const [name, expected] of Object.entries(files)) {
    const url = feedUrl({ repo, name })
    const served = await get(url, { accept: 'application/octet-stream, */*' })
    if (!served.ok) pending.push(`${url} answered ${describeResponse(served)} without credentials`)
    else if (served.text !== expected) pending.push(`${url} still serves an older copy`)
  }

  if (release) {
    // Only stable is ever latest: /releases/latest is what the website and
    // people follow, and a nightly there would put every stable reader on it.
    const pointerUrl = `https://github.com/${repo}/releases/latest`
    const pointer = await get(pointerUrl, { accept: 'application/json' })
    let pointerTag = null
    try {
      pointerTag = pointer.ok ? JSON.parse(pointer.text)?.tag_name ?? null : null
    } catch {
      pointerTag = null
    }
    if (release.channel === 'nightly') {
      if (pointerTag === release.tag) {
        settled.push(`${pointerUrl} resolves to ${release.tag}, a nightly. Publish nightlies with --latest=false.`)
      }
    } else if (pointerTag !== release.tag) {
      pending.push(
        `${pointerUrl} answered ${describeResponse(pointer)} and resolves to ${pointerTag ?? 'nothing'}, not ${release.tag}; ` +
          `re-run \`gh release edit ${release.tag} --latest\`.`,
      )
    }
  }
  return result()
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

// The upload and this check are seconds apart, so poll before giving up -- but
// only while every problem is one that propagation could still resolve.
export async function verifyPublicFeeds(options) {
  const { attempts = 8, delayMs = 10_000, wait = sleep } = options
  let problems = []
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const outcome = await checkPublicFeeds(options)
    problems = outcome.problems
    if (problems.length === 0 || !outcome.retryable) return problems
    if (attempt < attempts) await wait(delayMs)
  }
  return problems
}
