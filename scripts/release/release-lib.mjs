// The pure half of the release workflow: version arithmetic, naming and
// collecting the packaged files, the Tauri updater manifest, the manifest
// checks, the release body, and the unauthenticated reachability checks at the
// end of the file. Authentication and the API client live in release.mjs;
// nothing here reaches the network on its own -- the reachability checks take
// the fetch they use as an argument, so release-lib.test.mjs covers all of it
// offline.

const VERSION_RE = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/

export function parseVersion(raw) {
  const match = VERSION_RE.exec(String(raw).replace(/^v/, ''))
  if (!match) throw new Error(`Not a release version: ${raw}`)
  const [, major, minor, patch, pre] = match
  return { major: Number(major), minor: Number(minor), patch: Number(patch), pre: pre ?? null }
}

export function coreVersion(raw) {
  const { major, minor, patch } = parseVersion(raw)
  return `${major}.${minor}.${patch}`
}

export function compareCore(a, b) {
  const left = parseVersion(a)
  const right = parseVersion(b)
  return left.major - right.major || left.minor - right.minor || left.patch - right.patch
}

// The updater channel a version belongs to, and so the name of its manifest
// (`latest.json` / `nightly.json`). The app reads its own channel the same way
// (src-tauri/src/updates.rs): a build whose FIRST prerelease identifier is
// `nightly` follows nightlies, every other build follows stable. So the train
// is spelled there: a nightly is vX.Y.Z-nightly.DATE.RUN. A tag like
// v1.2.0-beta.1 would be a release no installed build could ever see, so it is
// refused here rather than published.
export const PRERELEASE_TRAINS = ['nightly']

export function channelForVersion(raw) {
  const { pre } = parseVersion(raw)
  if (pre === null) return 'latest'
  const train = pre.split('.')[0]
  if (PRERELEASE_TRAINS.includes(train)) return train
  throw new Error(
    `Prerelease ${raw} is not a nightly version. Installed nightly builds only follow ` +
      `tags shaped vX.Y.Z-nightly.DATE.RUN, so this release would reach nobody.`,
  )
}

// Numeric prerelease identifiers compare numerically in semver, so date then run
// number orders every nightly, including two on the same day.
export function prereleaseVersion(base, train, date, runNumber) {
  if (!PRERELEASE_TRAINS.includes(train)) throw new Error(`Unknown prerelease train ${train}`)
  if (!/^\d{8}$/.test(date)) throw new Error(`Prerelease date must be YYYYMMDD, got ${date}`)
  if (!Number.isInteger(Number(runNumber)) || Number(runNumber) < 1) {
    throw new Error(`Run number must be a positive integer, got ${runNumber}`)
  }
  return `${coreVersion(base)}-${train}.${date}.${Number(runNumber)}`
}

export function utcDateStamp(isoTimestamp) {
  const date = new Date(isoTimestamp)
  if (Number.isNaN(date.getTime())) throw new Error(`Not a timestamp: ${isoTimestamp}`)
  return date.toISOString().slice(0, 10).replaceAll('-', '')
}

// The released commit is recorded in the release body. A stable promotion
// rebuilds the commit a nightly shipped, and the nightly gate compares main
// with it; both read it back from here rather than trusting a tag.
const SOURCE_SHA_RE = /<!-- source-sha: ([0-9a-f]{40}) -->/

export function sourceShaMarker(sha) {
  if (!/^[0-9a-f]{40}$/.test(sha)) throw new Error(`Not a full commit sha: ${sha}`)
  return `<!-- source-sha: ${sha} -->`
}

export function sourceShaFromBody(body) {
  return SOURCE_SHA_RE.exec(body ?? '')?.[1] ?? null
}

// ---------------------------------------------------------------------------
// Where releases live.
//
// The stable updater endpoint in tauri.conf.json is the one place the releases
// repository is named. The workflow reads it from there and the app derives its
// nightly feed from the same string, so the two cannot disagree about where
// updates come from.

const STABLE_ENDPOINT_RE = /^https:\/\/github\.com\/([\w.-]+)\/([\w.-]+)\/releases\/latest\/download\/latest\.json$/

export function releasesRepoFromEndpoint(endpoint) {
  const match = STABLE_ENDPOINT_RE.exec(String(endpoint ?? ''))
  if (!match) {
    throw new Error(
      `The updater endpoint must be https://github.com/OWNER/REPO/releases/latest/download/latest.json, got ${endpoint}`,
    )
  }
  return `${match[1]}/${match[2]}`
}

// The file-name stem every packaged file shares: "Multivoice Tauri" ->
// "multivoice-tauri". GitHub rewrites spaces in asset names, so none are used.
export function productSlug(productName) {
  const slug = String(productName).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
  if (!slug) throw new Error(`Cannot name files after the product name ${JSON.stringify(productName)}`)
  return slug
}

// ---------------------------------------------------------------------------
// The packaged files.
//
// Tauri names its bundles after the product. Each package leg renames its
// files to `<slug>-<version>-<platform>...` so the legs can be gathered into
// one release without overwriting each other, and so the manifest check can
// tell the platforms apart by name.
//
// Keys are Tauri's updater platform names; the manifest must carry every one.
// There is no macOS entry: the Tauri app ships for Windows and Linux only, and
// Macs run the native apps in apps/macos (below). The macOS package legs build
// only the transcription host, so no darwin-* fragment is ever written, and an
// installed Tauri macOS build finds no update in the manifest.
export const PLATFORMS = {
  // The NSIS installer is also the updater's payload. There is no MSI: WiX
  // refuses a non-numeric prerelease, so it cannot package a nightly.
  'windows-x86_64': {
    label: 'windows-x64',
    installers: [],
    updater: '-setup.exe',
  },
  'linux-x86_64': {
    label: 'linux-x86_64',
    installers: [{ suffix: '.deb', name: 'deb' }],
    updater: '.AppImage',
  },
}

export function releaseFileName({ slug, version, platform, suffix }) {
  const target = PLATFORMS[platform]
  if (!target) throw new Error(`Unknown updater platform ${platform}`)
  return `${slug}-${version}-${target.label}${suffix}`
}

// What one package leg uploads, given every file under its bundle directory:
// each installer and the updater payload renamed, and the payload's signature
// (read by the caller into the manifest fragment, not uploaded). Anything
// missing or ambiguous throws: a leg that cannot say which file it built must
// not guess.
export function planCollect({ files, platform, slug, version }) {
  const target = PLATFORMS[platform]
  if (!target) throw new Error(`Unknown updater platform ${platform}`)
  const base = (file) => file.split(/[\\/]/).pop()
  const only = (suffix, what) => {
    const matches = files.filter((file) => base(file).endsWith(suffix) && !base(file).endsWith(`${suffix}.sig`))
    if (matches.length !== 1) {
      throw new Error(`Expected one ${what} (*${suffix}) for ${platform}, found ${matches.length}: ${matches.join(', ') || 'none'}`)
    }
    return matches[0]
  }

  const copies = target.installers.map(({ suffix, name }) => ({
    from: only(suffix, name),
    to: releaseFileName({ slug, version, platform, suffix }),
  }))
  const payload = only(target.updater, 'updater payload')
  const signature = `${payload}.sig`
  if (!files.includes(signature)) {
    throw new Error(`${payload} has no signature. Is TAURI_SIGNING_PRIVATE_KEY set, and createUpdaterArtifacts on?`)
  }
  const payloadName = releaseFileName({ slug, version, platform, suffix: target.updater })
  copies.push({ from: payload, to: payloadName })
  return { copies, payloadName, signature }
}

export function releaseAssetUrl({ repo, tag, name }) {
  return `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`
}

// One leg's share of the updater manifest. The publish job merges them.
export function manifestFragment({ platform, version, repo, tag, payloadName, signature }) {
  if (!PLATFORMS[platform]) throw new Error(`Unknown updater platform ${platform}`)
  const trimmed = String(signature ?? '').trim()
  if (!trimmed) throw new Error(`The ${platform} updater signature is empty`)
  return {
    version,
    platforms: { [platform]: { signature: trimmed, url: releaseAssetUrl({ repo, tag, name: payloadName }) } },
  }
}

// Every leg's fragment folded into the one manifest an installed build reads.
// Two legs claiming a platform, or disagreeing on the version, is a broken run.
export function mergeManifestFragments(fragments, { notes = '', pubDate }) {
  if (fragments.length === 0) throw new Error('No updater manifest fragments to merge')
  const version = fragments[0].version
  const platforms = {}
  for (const fragment of fragments) {
    if (fragment.version !== version) {
      throw new Error(`Updater fragments disagree on version: ${version} vs ${fragment.version}`)
    }
    for (const [platform, entry] of Object.entries(fragment.platforms ?? {})) {
      if (platforms[platform]) throw new Error(`Two package legs both claim ${platform}`)
      platforms[platform] = entry
    }
  }
  return { version, notes, pub_date: pubDate, platforms }
}

export function manifestName(channel) {
  return `${channel}.json`
}

// What an installed app needs from the updater manifest. Returns the problems,
// empty when the manifest is usable.
export function checkManifest(text, { version, repo, tag, assetNames }) {
  let doc
  try {
    doc = JSON.parse(text)
  } catch (error) {
    return [`does not parse: ${error.message}`]
  }
  const problems = []
  if (doc?.version !== version) problems.push(`names version ${doc?.version}, expected ${version}`)
  const prefix = `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/`
  for (const platform of Object.keys(PLATFORMS)) {
    const entry = doc?.platforms?.[platform]
    if (!entry) {
      problems.push(`has no ${platform} entry, so those installs are never offered it`)
      continue
    }
    if (typeof entry.signature !== 'string' || entry.signature.trim() === '') {
      problems.push(`has no ${platform} signature, and the updater refuses an unsigned payload`)
    }
    const url = String(entry.url ?? '')
    if (!url.startsWith(prefix)) {
      problems.push(`points ${platform} at ${url}, not at ${tag} on ${repo}`)
      continue
    }
    const name = decodeURIComponent(url.slice(prefix.length))
    if (!assetNames.includes(name)) problems.push(`points ${platform} at ${name}, which is not on the release`)
    else if (!name.endsWith(PLATFORMS[platform].updater)) problems.push(`points ${platform} at ${name}, not a *${PLATFORMS[platform].updater}`)
  }
  return problems
}

// ---------------------------------------------------------------------------
// The other products on the same release.
//
// One train, one version, one release: every nightly and stable carries the
// Tauri app above (Windows and Linux), the standalone transcription host and
// the two native macOS apps, all built from the same commit.
//
// The transcription host (`cargo build --release --bin transcription-host`)
// ships as one archive per platform, macOS included: a folder named like the
// archive holding the binary, src-tauri/src/host/PROTOCOL.md, the packaging/host
// service files, LICENSE and THIRD_PARTY_NOTICES.md. zip on Windows; tar.gz
// elsewhere, which keeps the binary executable. release.yml's "Package
// transcription host" step builds these names; keep the two in step.
export const HOST_TARGETS = {
  'linux-x64': '.tar.gz',
  'linux-arm64': '.tar.gz',
  'windows-x64': '.zip',
  'macos-arm64': '.tar.gz',
  'macos-x64': '.tar.gz',
}

export function hostArchiveName({ slug, version, target }) {
  const extension = HOST_TARGETS[target]
  if (!extension) throw new Error(`Unknown transcription host target ${target}`)
  return `${slug}-${version}-transcription-host-${target}${extension}`
}

// The native macOS apps (apps/macos), packaged by apps/macos/scripts/release.sh:
// the dictation client as <Name>-<version>-macos-arm64.{zip,dmg}, and the
// transcription server (`release.sh --app server`) as
// <Name>-Server-<version>-macos-arm64.{zip,dmg}. <Name> is MV_DISPLAY_NAME in
// apps/macos/Config/Base.xcconfig, the one place the app's name is defined. A
// build made without the Apple secrets is ad-hoc signed and says so in its
// name (-unsigned), so nobody mistakes it for a notarized one.
export const MAC_APP_EXTENSIONS = ['.zip', '.dmg']

export function macAppFileName({ appName, version, signed = true, extension }) {
  if (!MAC_APP_EXTENSIONS.includes(extension)) throw new Error(`Unknown macOS app package ${extension}`)
  const stem = String(appName ?? '').trim()
  if (!stem || /\s/.test(stem)) throw new Error(`Cannot name files after the macOS app name ${JSON.stringify(appName)}`)
  return `${stem}-${version}-macos-arm64${signed ? '' : '-unsigned'}${extension}`
}

export function macAppNameFromXcconfig(text) {
  return /^MV_DISPLAY_NAME = (\S.*?)\s*$/m.exec(String(text ?? ''))?.[1] ?? null
}

// The file-name stem of the server app ("Fairspoken Server.app"): the client's
// name with -Server, as release.sh --app server names its files.
export function macServerName(appName) {
  return appName ? `${appName}-Server` : null
}

// The native macOS apps a release carries, by file-name stem and description.
// None with no macAppName (a commit from before apps/macos existed).
function macApps(macAppName) {
  if (!macAppName) return []
  return [
    { stem: macAppName, what: 'the native macOS app' },
    { stem: macServerName(macAppName), what: 'the native macOS server app' },
  ]
}

// Installers a complete release carries, beside the manifest. Exact names, not
// suffixes: the client's <Name>-<version>-macos-arm64.dmg and the server's
// <Name>-Server-<version>-macos-arm64.dmg end the same way, and neither may
// stand in for the other. There is no Tauri macOS installer to ask for.
export function missingInstallers(assetNames, { slug, version, macAppName = null }) {
  const has = (name) => assetNames.includes(name)
  const tauri = (platform, suffix) => releaseFileName({ slug, version, platform, suffix })
  const missing = []
  if (!has(tauri('windows-x86_64', '-setup.exe'))) missing.push(`a Windows -setup.exe (${tauri('windows-x86_64', '-setup.exe')})`)
  if (!has(tauri('linux-x86_64', '.AppImage'))) missing.push(`an .AppImage (${tauri('linux-x86_64', '.AppImage')})`)
  for (const target of Object.keys(HOST_TARGETS)) {
    const name = hostArchiveName({ slug, version, target })
    if (!has(name)) missing.push(`the ${target} transcription host (${name})`)
  }
  for (const { stem, what } of macApps(macAppName)) {
    for (const extension of MAC_APP_EXTENSIONS) {
      const signed = macAppFileName({ appName: stem, version, extension })
      if (!has(signed) && !has(macAppFileName({ appName: stem, version, signed: false, extension }))) {
        missing.push(`${what} ${extension} (${signed})`)
      }
    }
  }
  return missing
}

// The native macOS app packages on a release that are ad-hoc signed.
export function unsignedMacApps(assetNames, { version, macAppName }) {
  return macApps(macAppName)
    .flatMap(({ stem }) => MAC_APP_EXTENSIONS.map((extension) => macAppFileName({ appName: stem, version, signed: false, extension })))
    .filter((name) => assetNames.includes(name))
}

// The release body. Commit subjects are listed only when the source repository
// is public, so a private repository's commit messages never reach a release.
export function buildReleaseNotes({ productName, version, channel, sourceRepo, sha, sourcePrivate, commits = [] }) {
  const lines = [
    channel === 'nightly'
      ? `Nightly build of ${productName} ${version}. Installed nightly builds update to newer nightlies.`
      : `${productName} ${version}.`,
  ]
  if (!sourcePrivate) {
    lines.push('', `Built from [\`${sha.slice(0, 12)}\`](https://github.com/${sourceRepo}/commit/${sha}).`)
    if (commits.length > 0) {
      const shown = commits.slice(0, 100)
      lines.push('', '### Changes', '')
      for (const commit of shown) {
        lines.push(`- ${commit.subject} ([\`${commit.sha.slice(0, 7)}\`](https://github.com/${sourceRepo}/commit/${commit.sha}))`)
      }
      if (commits.length > shown.length) lines.push(`- ...and ${commits.length - shown.length} more`)
    }
  }
  lines.push('', sourceShaMarker(sha))
  return `${lines.join('\n')}\n`
}

// ---------------------------------------------------------------------------
// What an installed build can actually reach.
//
// Every other check in the release job runs with the job's token, which reads a
// private or misnamed repository perfectly happily. The questions below are
// asked the way the shipped updater asks them: with no credential at all, and
// against github.com rather than api.github.com -- the app stays off the API
// deliberately, because an anonymous API budget is per address and shared with
// everything else on the user's network.

const ANONYMOUS_USER_AGENT = 'multivoice-release'

// fetch is a parameter so the tests can read back exactly what was sent: the
// point of this function is the headers it does NOT carry, and an ambient
// GH_TOKEN picked up by a helper somewhere is the failure it exists to rule
// out. Nothing a caller passes is merged into these headers.
export function anonymousGet(fetchImpl = fetch) {
  return async (url, { accept = 'application/json' } = {}) => {
    let response
    try {
      response = await fetchImpl(url, {
        headers: { accept, 'user-agent': ANONYMOUS_USER_AGENT },
        redirect: 'follow',
      })
    } catch (error) {
      // A DNS blip at the end of a long matrix should be retried, not reported
      // as "the repository is private".
      return { ok: false, status: 0, text: '', error: error.message }
    }
    return { ok: response.ok, status: response.status, text: response.ok ? await response.text() : '' }
  }
}

// The URLs an installed build reads. A stable build asks for `stableManifest`,
// which GitHub redirects to the newest release not marked prerelease. A nightly
// build reads the feed, takes the first nightly tag in it, and downloads that
// release's `nightly.json` -- `manifest` below.
export function updaterUrls({ repo, tag, channel }) {
  const releases = `https://github.com/${repo}/releases`
  return {
    feed: `${releases}.atom`,
    latestPointer: `${releases}/latest`,
    stableManifest: `${releases}/latest/download/${manifestName('latest')}`,
    manifest: `${releases}/download/${encodeURIComponent(tag)}/${manifestName(channel)}`,
  }
}

// Feed entries newest first, as tags. The app reads each entry's link and takes
// the last path segment, so this parses what it parses.
const FEED_TAG_RE = /\/releases\/tag\/([^"/<]+)/g

export function feedTags(atomXml) {
  return [...String(atomXml).matchAll(FEED_TAG_RE)].map(([, raw]) => {
    try {
      return decodeURIComponent(raw)
    } catch {
      return raw
    }
  })
}

// The channel of a tag that may be neither of ours: the feed carries whatever
// anyone has ever published to the repository.
function tagChannel(tag) {
  try {
    return channelForVersion(tag)
  } catch {
    return null
  }
}

// How many nightly releases stay on the repository. The newest is what the
// nightly gate and a stable promotion read back, and what installed nightly
// builds update to; the two before it are there to go back to. Older ones are
// history the tags already keep.
export const NIGHTLIES_KEPT = 3

// The tags of the published nightlies past the newest `keep`, oldest last.
// Stable releases, drafts, and anything that is not a nightly are never named.
export function nightliesToPrune(releases, keep = NIGHTLIES_KEPT) {
  if (!Number.isInteger(keep) || keep < 1) throw new Error(`Keep at least one nightly, not ${keep}`)
  return releases
    .filter((release) => !release.draft && release.published_at && tagChannel(release.tag_name) === 'nightly')
    .sort((a, b) => Date.parse(b.published_at) - Date.parse(a.published_at))
    .slice(keep)
    .map((release) => release.tag_name)
}

const describeResponse = (response) => (response.error ? `failed: ${response.error}` : `HTTP ${response.status}`)

// One unauthenticated pass over a published release. Returns the problems and
// whether waiting could still fix them: GitHub serves the feed and the release
// downloads through a cache that can lag a publish by seconds, while a private
// repository or a nightly stacked behind a newer nightly answers the same way
// forever and polling those only delays the failure.
export async function checkPublicRelease({ repo, tag, channel, version, assetNames = [], get }) {
  const urls = updaterUrls({ repo, tag, channel })
  // Which channel a user's updater puts this release in is decided by the tag
  // and nothing else, so the checks that turn on it read the tag rather than
  // the caller's channel. A tag that parses as no train falls through to the
  // stable checks, which are the stricter set.
  const isNightly = tagChannel(tag) === 'nightly'
  const pending = []
  const settled = []
  const result = () => ({ problems: [...pending, ...settled], retryable: settled.length === 0 && pending.length > 0 })

  const feed = await get(urls.feed, { accept: 'application/xml' })
  if (!feed.ok) {
    settled.push(
      `${urls.feed} answered ${describeResponse(feed)} without credentials. Installed nightly builds find ` +
        `every release through it, and a repository that hides it hides its downloads too. Check ${repo} is ` +
        `public, and that the updater endpoint in tauri.conf.json names the repository the release went to.`,
    )
    return result()
  }

  const tags = feedTags(feed.text)
  if (!tags.includes(tag)) {
    pending.push(
      `${tag} is not in ${urls.feed}, which lists ${tags.slice(0, 5).join(', ') || 'no releases at all'}. ` +
        `A release still in draft, or published to a different repository, is invisible in exactly this way.`,
    )
  } else if (isNightly) {
    // An installed nightly build takes the FIRST nightly entry in the feed, not
    // the highest version, so a nightly published behind a newer one reaches
    // nobody however correct its own assets are.
    const newest = tags.find((candidate) => tagChannel(candidate) === 'nightly')
    if (newest !== tag) {
      settled.push(
        `${urls.feed} lists ${newest} above ${tag}. Installed nightly builds follow the first ` +
          `nightly entry in the feed, so they would be offered ${newest} instead of this release.`,
      )
    }
  }

  // Only a stable build reads through this pointer, but every channel has
  // something to prove about it: that a stable release is what it resolves to,
  // and that a nightly is not.
  const pointer = await get(urls.latestPointer, { accept: 'application/json' })
  const pointerTag = pointer.ok ? parseTagName(pointer.text) : null
  if (isNightly) {
    if (pointerTag === tag) {
      settled.push(
        `${urls.latestPointer} resolves to ${tag}, a nightly. Every installed stable build resolves its ` +
          `next version through that URL, so all of them would be offered a nightly build. Publish ` +
          `nightlies with --latest=false.`,
      )
    }
  } else if (pointerTag !== tag) {
    pending.push(
      `${urls.latestPointer} answered ${describeResponse(pointer)} and resolves to ${pointerTag ?? 'nothing'}, ` +
        `not ${tag}. Installed stable builds read their next version from there and would not be offered ` +
        `this release; re-run \`gh release edit ${tag} --latest\`.`,
    )
  }

  // The bytes the CDN hands a user, which is where a half-finished upload or a
  // stale cached object shows up. A stable is also read through the exact URL
  // the app requests, which only answers once the pointer above has moved.
  const manifestUrls = isNightly ? [urls.manifest] : [urls.manifest, urls.stableManifest]
  for (const url of manifestUrls) {
    const manifest = await get(url, { accept: 'application/json, application/octet-stream, */*' })
    if (!manifest.ok) {
      pending.push(
        `${url} answered ${describeResponse(manifest)} without credentials. The updater downloads that ` +
          `exact URL to learn about the release, and finds no update without it.`,
      )
      continue
    }
    // Through the stable pointer, another version is the pointer lagging
    // (reported above), which waiting can fix; everything else in that older
    // manifest is about a different release.
    const served = manifestVersion(manifest.text)
    if (url === urls.stableManifest && served !== version) {
      pending.push(`${url} names version ${served}, expected ${version}`)
      continue
    }
    for (const problem of checkManifest(manifest.text, { version, repo, tag, assetNames })) {
      settled.push(`${url} ${problem}`)
    }
  }

  return result()
}

function manifestVersion(text) {
  try {
    return JSON.parse(text)?.version ?? null
  } catch {
    return null
  }
}

// github.com answers /releases/latest with the release as JSON when asked for
// it; the tag is read straight back out of that.
function parseTagName(text) {
  try {
    return JSON.parse(text)?.tag_name ?? null
  } catch {
    return null
  }
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

// The publish step and this check are seconds apart, so poll before giving up --
// but only while every problem is one that propagation could still resolve.
export async function verifyPublicRelease(options) {
  const { attempts = 6, delayMs = 10_000, wait = sleep } = options
  let problems = []
  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const outcome = await checkPublicRelease(options)
    problems = outcome.problems
    if (problems.length === 0 || !outcome.retryable) return problems
    if (attempt < attempts) await wait(delayMs)
  }
  return problems
}
