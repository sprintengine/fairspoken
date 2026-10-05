// The pure half of the release workflow: version and build-number arithmetic,
// naming and collecting the packaged files, the per-release metadata the update
// feeds are rendered from (desktop updater manifest, host manifest, Sparkle
// metadata), the release checks, the release body, and the secret groups.
// Authentication and the API client live in release.mjs; the update feeds in
// feeds.mjs. Nothing here reaches the network, so release-lib.test.mjs covers
// all of it offline.

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

// The update channel a version belongs to: `stable` or `nightly`. Every app
// derives its default channel the same way (contract §1): a build whose FIRST
// prerelease identifier is `nightly` is a nightly, a build with none is stable.
// So the train is spelled here: a nightly is vX.Y.Z-nightly.DATE.RUN. A tag
// like v1.2.0-beta.1 would be a release no feed carries, so it is refused here
// rather than published.
export const PRERELEASE_TRAINS = ['nightly']
export const CHANNELS = ['stable', 'nightly']

export function channelForVersion(raw) {
  const { pre } = parseVersion(raw)
  if (pre === null) return 'stable'
  const train = pre.split('.')[0]
  if (PRERELEASE_TRAINS.includes(train)) return train
  throw new Error(
    `Prerelease ${raw} is not a nightly version. The update feeds only carry stable ` +
      `vX.Y.Z and nightly vX.Y.Z-nightly.DATE.RUN tags, so this release would reach nobody.`,
  )
}

// The channel of a tag that may be neither of ours (update-feeds, a stranger's).
export function tagChannel(tag) {
  try {
    return channelForVersion(tag)
  } catch {
    return null
  }
}

// Numeric prerelease identifiers compare numerically in semver, so date then run
// number orders every nightly, including two on the same day. The run number
// orders nightlies against each other only; nothing compares it with a build
// number.
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

// The build number every product of one release shares: the UTC minute the
// release was resolved, YYYYMMDDHHMM (contract §1). It is the macOS apps'
// CFBundleVersion and so what Sparkle compares: it only grows, whatever repo or
// run counter a build came from, and a stable promoted from a nightly is always
// cut after it, so it is always above that nightly.
export function buildNumber(isoTimestamp) {
  const date = new Date(isoTimestamp)
  if (Number.isNaN(date.getTime())) throw new Error(`Not a timestamp: ${isoTimestamp}`)
  return date.toISOString().slice(0, 16).replace(/[-T:]/g, '')
}

export function isBuildNumber(value) {
  const match = /^(\d{4})(\d{2})(\d{2})(\d{2})(\d{2})$/.exec(String(value ?? ''))
  if (!match) return false
  const [, year, month, day, hour, minute] = match
  const iso = `${year}-${month}-${day}T${hour}:${minute}:00.000Z`
  const date = new Date(iso)
  return !Number.isNaN(date.getTime()) && date.toISOString() === iso
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
// The updater endpoint in tauri.conf.json names the releases repository, and
// the release scripts refuse to publish anywhere else, so the workflow and
// installed builds cannot disagree about it. The endpoint is the stable desktop
// feed on the update-feeds release (contract §2); the desktop app derives the
// nightly feed from it by swapping stable for nightly, so nothing else is
// accepted.

const ENDPOINT_RE = /^https:\/\/github\.com\/([\w.-]+)\/([\w.-]+)\/releases\/download\/update-feeds\/desktop-stable\.json$/

export function releasesRepoFromEndpoint(endpoint) {
  const match = ENDPOINT_RE.exec(String(endpoint ?? ''))
  if (!match) {
    throw new Error(
      `The updater endpoint must be https://github.com/OWNER/REPO/releases/download/update-feeds/desktop-stable.json, got ${endpoint}`,
    )
  }
  return `${match[1]}/${match[2]}`
}

export function releaseAssetUrl({ repo, tag, name }) {
  return `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(name)}`
}

export function releasePageUrl({ repo, tag }) {
  return `https://github.com/${repo}/releases/tag/${encodeURIComponent(tag)}`
}

// The file-name stem every packaged file shares: "Fairspoken" ->
// "fairspoken". GitHub rewrites spaces in asset names, so none are used.
export function productSlug(productName) {
  const slug = String(productName).toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '')
  if (!slug) throw new Error(`Cannot name files after the product name ${JSON.stringify(productName)}`)
  return slug
}

// ---------------------------------------------------------------------------
// The Tauri desktop app.
//
// Tauri names its bundles after the product. Each package leg renames its files
// to `<slug>-<version>-<label>...` so the legs can share one release, and
// writes its share of the desktop updater manifest beside them.
//
// Legs are keyed by the package job's matrix platform; each payload by the
// updater platform key it fills in desktop-<channel>.json. The Tauri updater
// looks up `<os>-<arch>-<installer>` first (linux-x86_64-deb for a .deb
// install) and then `<os>-<arch>`, so a .deb install updates from the .deb and
// an AppImage from the AppImage. There is no macOS leg: Macs run the native
// apps, which update through Sparkle.
export const DESKTOP_LEGS = {
  // The NSIS installer is also the updater's payload. There is no MSI: WiX
  // refuses a non-numeric prerelease, so it cannot package a nightly.
  'windows-x86_64': {
    label: 'windows-x64',
    payloads: [{ platform: 'windows-x86_64', suffix: '-setup.exe', what: 'NSIS installer' }],
  },
  'linux-x86_64': {
    label: 'linux-x86_64',
    payloads: [
      { platform: 'linux-x86_64', suffix: '.AppImage', what: 'AppImage' },
      { platform: 'linux-x86_64-deb', suffix: '.deb', what: 'deb' },
    ],
  },
}

// Every key desktop-<channel>.json carries, with the suffix of the file it
// points at.
export const DESKTOP_PLATFORMS = Object.fromEntries(
  Object.values(DESKTOP_LEGS).flatMap((leg) => leg.payloads.map((payload) => [payload.platform, payload.suffix])),
)

export function releaseFileName({ slug, version, platform, suffix }) {
  const leg = DESKTOP_LEGS[platform]
  if (!leg) throw new Error(`Unknown desktop platform ${platform}`)
  return `${slug}-${version}-${leg.label}${suffix}`
}

// What one package leg uploads, given every file under its bundle directory:
// each payload renamed, and the path of its signature (read by the caller into
// the manifest fragment, not uploaded). Anything missing or ambiguous throws: a
// leg that cannot say which file it built must not guess.
export function planCollect({ files, platform, slug, version }) {
  const leg = DESKTOP_LEGS[platform]
  if (!leg) throw new Error(`Unknown desktop platform ${platform}`)
  const base = (file) => file.split(/[\\/]/).pop()
  const copies = []
  const payloads = []
  for (const payload of leg.payloads) {
    const matches = files.filter((file) => base(file).endsWith(payload.suffix))
    if (matches.length !== 1) {
      throw new Error(`Expected one ${payload.what} (*${payload.suffix}) for ${platform}, found ${matches.length}: ${matches.join(', ') || 'none'}`)
    }
    const [file] = matches
    const signature = `${file}.sig`
    if (!files.includes(signature)) {
      throw new Error(`${file} has no signature. Is TAURI_SIGNING_PRIVATE_KEY set, and createUpdaterArtifacts on?`)
    }
    const name = releaseFileName({ slug, version, platform, suffix: payload.suffix })
    copies.push({ from: file, to: name })
    payloads.push({ platform: payload.platform, file, name, signature })
  }
  return { copies, payloads }
}

function trimmedSignature(signature, what) {
  const trimmed = String(signature ?? '').trim()
  if (!trimmed) throw new Error(`The ${what} signature is empty`)
  return trimmed
}

// One desktop leg's share of desktop-manifest.json. The publish job merges them.
export function desktopFragment({ version, repo, tag, payloads }) {
  const platforms = {}
  for (const { platform, name, signature } of payloads) {
    if (!DESKTOP_PLATFORMS[platform]) throw new Error(`Unknown desktop platform ${platform}`)
    platforms[platform] = { signature: trimmedSignature(signature, platform), url: releaseAssetUrl({ repo, tag, name }) }
  }
  return { version, platforms }
}

// Every leg's fragment folded into one manifest. Two legs claiming a platform,
// or disagreeing on the version, is a broken run.
export function mergeFragments(fragments, extra = {}) {
  if (fragments.length === 0) throw new Error('No manifest fragments to merge')
  const version = fragments[0].version
  const platforms = {}
  for (const fragment of fragments) {
    if (fragment.version !== version) throw new Error(`Fragments disagree on version: ${version} vs ${fragment.version}`)
    for (const [platform, entry] of Object.entries(fragment.platforms ?? {})) {
      if (platforms[platform]) throw new Error(`Two package legs both claim ${platform}`)
      platforms[platform] = entry
    }
  }
  return { version, ...extra, platforms }
}

// The per-release metadata assets. The update feeds are rendered from these,
// never appended to, so a feed only ever names releases that still exist.
export const DESKTOP_MANIFEST = 'desktop-manifest.json'
export const HOST_MANIFEST = 'host-manifest.json'
export const FRAGMENT_RE = /^fragment-.+\.json$/

export function desktopFragmentName(platform) {
  return `fragment-desktop-${platform}.json`
}

export function hostFragmentName(target) {
  return `fragment-host-${target}.json`
}

function parseDoc(textOrDoc) {
  if (typeof textOrDoc !== 'string') return { doc: textOrDoc }
  try {
    return { doc: JSON.parse(textOrDoc) }
  } catch (error) {
    return { error: `does not parse: ${error.message}` }
  }
}

// Checks one platform entry's url: it names an asset on `tag` of `repo` that
// exists and ends with `suffix`. Returns the problem, or null.
function urlProblem(platform, url, { repo, tag, assetNames, suffix }) {
  const prefix = `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/`
  if (!String(url ?? '').startsWith(prefix)) return `points ${platform} at ${url}, not at ${tag} on ${repo}`
  const name = decodeURIComponent(String(url).slice(prefix.length))
  if (!assetNames.includes(name)) return `points ${platform} at ${name}, which is not on the release`
  if (!name.endsWith(suffix)) return `points ${platform} at ${name}, not a *${suffix}`
  return null
}

// What an installed desktop app needs from desktop-manifest.json (and so from
// desktop-<channel>.json). Returns the problems, empty when it is usable.
export function checkDesktopManifest(textOrDoc, { version, repo, tag, assetNames }) {
  const { doc, error } = parseDoc(textOrDoc)
  if (error) return [error]
  const problems = []
  if (doc?.version !== version) problems.push(`names version ${doc?.version}, expected ${version}`)
  for (const [platform, suffix] of Object.entries(DESKTOP_PLATFORMS)) {
    const entry = doc?.platforms?.[platform]
    if (!entry) {
      problems.push(`has no ${platform} entry, so those installs are never offered it`)
      continue
    }
    if (typeof entry.signature !== 'string' || entry.signature.trim() === '') {
      problems.push(`has no ${platform} signature, and the updater refuses an unsigned payload`)
    }
    const problem = urlProblem(platform, entry.url, { repo, tag, assetNames, suffix })
    if (problem) problems.push(problem)
  }
  return problems
}

// ---------------------------------------------------------------------------
// The standalone transcription host.
//
// `cargo build --release --bin transcription-host` ships as one archive per
// platform, built by release.yml's "Package transcription host" step: a single
// top-level folder named like the archive (without the extension) holding the
// binary, PROTOCOL.md, the packaging/host service files, LICENSE and
// THIRD_PARTY_NOTICES.md. zip on Windows; tar.gz elsewhere, which keeps the
// binary executable. docs/releasing.md documents the layout for the host's
// self-updater, which extracts `binary` below; keep the three in step.
export const HOST_TARGETS = {
  'linux-x64': { extension: '.tar.gz', platform: 'linux-x86_64', binary: 'transcription-host' },
  'linux-arm64': { extension: '.tar.gz', platform: 'linux-aarch64', binary: 'transcription-host' },
  'windows-x64': { extension: '.zip', platform: 'windows-x86_64', binary: 'transcription-host.exe' },
  'macos-arm64': { extension: '.tar.gz', platform: 'darwin-aarch64', binary: 'transcription-host' },
  'macos-x64': { extension: '.tar.gz', platform: 'darwin-x86_64', binary: 'transcription-host' },
}

// Every key host-<channel>.json carries (contract §5).
export const HOST_PLATFORMS = Object.values(HOST_TARGETS).map((target) => target.platform)

function hostTarget(target) {
  const entry = HOST_TARGETS[target]
  if (!entry) throw new Error(`Unknown transcription host target ${target}`)
  return entry
}

export function hostArchiveName({ slug, version, target }) {
  return `${slug}-${version}-transcription-host-${target}${hostTarget(target).extension}`
}

// Where the binary sits inside the archive: `<folder>/<binary>`.
export function hostArchiveLayout({ slug, version, target }) {
  const { extension, binary, platform } = hostTarget(target)
  const folder = `${slug}-${version}-transcription-host-${target}`
  return { archive: `${folder}${extension}`, folder, binary: `${folder}/${binary}`, platform, format: extension.slice(1) }
}

// One host leg's share of host-manifest.json.
export function hostFragment({ slug, version, repo, tag, target, sha256, signature }) {
  const layout = hostArchiveLayout({ slug, version, target })
  if (!/^[0-9a-f]{64}$/.test(String(sha256))) throw new Error(`Not a sha256: ${sha256}`)
  return {
    version,
    platforms: {
      [layout.platform]: {
        url: releaseAssetUrl({ repo, tag, name: layout.archive }),
        sha256,
        signature: trimmedSignature(signature, `${target} host archive`),
        format: layout.format,
      },
    },
  }
}

export function checkHostManifest(textOrDoc, { version, repo, tag, assetNames }) {
  const { doc, error } = parseDoc(textOrDoc)
  if (error) return [error]
  const problems = []
  if (doc?.version !== version) problems.push(`names version ${doc?.version}, expected ${version}`)
  for (const target of Object.values(HOST_TARGETS)) {
    const entry = doc?.platforms?.[target.platform]
    if (!entry) {
      problems.push(`has no ${target.platform} entry`)
      continue
    }
    const format = target.extension.slice(1)
    if (!/^[0-9a-f]{64}$/.test(String(entry.sha256))) problems.push(`has no sha256 for ${target.platform}`)
    if (typeof entry.signature !== 'string' || entry.signature.trim() === '') problems.push(`has no ${target.platform} signature`)
    if (entry.format !== format) problems.push(`says ${target.platform} is ${entry.format}, not ${format}`)
    const problem = urlProblem(target.platform, entry.url, { repo, tag, assetNames, suffix: target.extension })
    if (problem) problems.push(problem)
  }
  return problems
}

// ---------------------------------------------------------------------------
// The native macOS apps (apps/macos), packaged by apps/macos/scripts/release.sh:
// the dictation client as <Name>-<version>-macos-arm64.{zip,dmg}, and the
// transcription server (`release.sh --app server`) as
// <Name>-Server-<version>-macos-arm64.{zip,dmg}. <Name> is MV_DISPLAY_NAME in
// apps/macos/Config/Base.xcconfig, the one place the app's name is defined. A
// build made without the Apple secrets is ad-hoc signed and says so in its
// name (-unsigned), so nobody mistakes it for a notarized one.
//
// The .zip (ditto --sequesterRsrc --keepParent of the stapled app) is Sparkle's
// enclosure; the DMG is for people. A notarized build signed with
// SPARKLE_ED_PRIVATE_KEY also carries sparkle-<app>.json (SPARKLE_APPS below),
// the facts its appcast item is rendered from.
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

// The two Sparkle-updated apps. The asset names are the contract (§2): the
// appcast URLs are compiled into every installed app, so they do not follow a
// rename of the display name. release.sh writes `metadata` (by --app kind).
export const SPARKLE_APPS = [
  {
    kind: 'client',
    title: 'Fairspoken',
    bundleId: 'ie.fairspoken.mac',
    metadata: 'sparkle-fairspoken.json',
    appcast: 'appcast-fairspoken.xml',
    stem: (macAppName) => macAppName,
  },
  {
    kind: 'server',
    title: 'Fairspoken Server',
    bundleId: 'ie.fairspoken.server',
    metadata: 'sparkle-fairspoken-server.json',
    appcast: 'appcast-fairspoken-server.xml',
    stem: (macAppName) => macServerName(macAppName),
  },
]

// What an appcast item needs from sparkle-<app>.json, checked against the
// release it sits on (`assets`: [{ name, size }]). Returns the problems.
export function checkSparkleMetadata(textOrDoc, { app, version, assets }) {
  const { doc, error } = parseDoc(textOrDoc)
  if (error) return [error]
  const problems = []
  if (doc?.bundleId !== app.bundleId) problems.push(`is for ${doc?.bundleId}, not ${app.bundleId}`)
  if (doc?.shortVersionString !== version) problems.push(`names version ${doc?.shortVersionString}, expected ${version}`)
  if (!isBuildNumber(doc?.version)) problems.push(`has build ${doc?.version}, not a YYYYMMDDHHMM build number`)
  if (!/^\d+(\.\d+){0,2}$/.test(String(doc?.minimumSystemVersion ?? ''))) problems.push('has no minimumSystemVersion')
  if (Buffer.from(String(doc?.edSignature ?? ''), 'base64').length !== 64) problems.push('has no 64-byte edSignature')
  if (!Number.isInteger(doc?.length) || doc.length <= 0) problems.push('has no length')
  const asset = assets.find((candidate) => candidate.name === doc?.file)
  if (!asset) {
    problems.push(`names ${doc?.file}, which is not on the release`)
  } else if (!String(doc.file).endsWith('.zip') || /-(unsigned|unnotarized)\.zip$/.test(doc.file)) {
    problems.push(`names ${doc.file}, not a notarized .zip`)
  } else if (asset.size !== undefined && asset.size !== doc.length) {
    problems.push(`says ${doc.file} is ${doc.length} bytes, the release has ${asset.size}`)
  }
  return problems
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

// Installers and metadata a complete release carries. Exact names, not
// suffixes: the client's <Name>-<version>-macos-arm64.dmg and the server's
// <Name>-Server-<version>-macos-arm64.dmg end the same way, and neither may
// stand in for the other. There is no Tauri macOS installer to ask for.
export function missingInstallers(assetNames, { slug, version, macAppName = null }) {
  const has = (name) => assetNames.includes(name)
  const missing = []
  for (const leg of Object.keys(DESKTOP_LEGS)) {
    for (const { suffix, what } of DESKTOP_LEGS[leg].payloads) {
      const name = releaseFileName({ slug, version, platform: leg, suffix })
      if (!has(name)) missing.push(`the desktop ${what} (${name})`)
    }
  }
  if (!has(DESKTOP_MANIFEST)) missing.push(`the desktop updater manifest (${DESKTOP_MANIFEST})`)
  for (const target of Object.keys(HOST_TARGETS)) {
    const name = hostArchiveName({ slug, version, target })
    if (!has(name)) missing.push(`the ${target} transcription host (${name})`)
  }
  if (!has(HOST_MANIFEST)) missing.push(`the transcription host manifest (${HOST_MANIFEST})`)
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

// The notarized native apps on a release that carry no Sparkle metadata, and so
// will not appear in their appcast.
export function macAppsWithoutSparkle(assetNames, { version, macAppName }) {
  if (!macAppName) return []
  return SPARKLE_APPS.filter(
    (app) =>
      assetNames.includes(macAppFileName({ appName: app.stem(macAppName), version, extension: '.zip' })) &&
      !assetNames.includes(app.metadata),
  ).map((app) => app.title)
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
// Nightly retention.
//
// How many nightly releases stay on the repository. The newest is what the
// nightly gate and a stable promotion read back, and what installed nightly
// builds update to; the two before it are there to go back to, and are the
// nightly items of each appcast. Older ones are history the tags already keep.
export const NIGHTLIES_KEPT = 3

// The tags of the published nightlies past the newest `keep`, oldest last.
// Stable releases, drafts, the update-feeds release and anything else that is
// not a nightly are never named.
export function nightliesToPrune(releases, keep = NIGHTLIES_KEPT) {
  if (!Number.isInteger(keep) || keep < 1) throw new Error(`Keep at least one nightly, not ${keep}`)
  return releases
    .filter((release) => !release.draft && release.published_at && tagChannel(release.tag_name) === 'nightly')
    .sort((a, b) => Date.parse(b.published_at) - Date.parse(a.published_at))
    .slice(keep)
    .map((release) => release.tag_name)
}

// ---------------------------------------------------------------------------
// Secrets.
//
// Checked in the resolve job, before anything is built or published, from
// presence flags only (the workflow passes `secrets.X != ''`, never a value).
// Each optional group is all-or-none: a partial set is a misconfiguration that
// fails the run, an empty one ships that platform unsigned with a warning.
export const SECRET_GROUPS = [
  {
    id: 'tauri',
    required: true,
    names: ['TAURI_SIGNING_PRIVATE_KEY'],
    purpose: 'signs the desktop updater payloads and the host archives',
  },
  {
    id: 'apple',
    names: ['CSC_LINK', 'CSC_KEY_PASSWORD', 'APPLE_ID', 'APPLE_APP_SPECIFIC_PASSWORD', 'APPLE_TEAM_ID'],
    none: 'the native macOS apps ship ad-hoc signed (-unsigned) with no appcast entry, and the macOS hosts are not notarized',
  },
  {
    id: 'azure',
    names: [
      'AZURE_TENANT_ID',
      'AZURE_CLIENT_ID',
      'AZURE_CLIENT_SECRET',
      'AZURE_TRUSTED_SIGNING_ENDPOINT',
      'AZURE_TRUSTED_SIGNING_ACCOUNT_NAME',
      'AZURE_TRUSTED_SIGNING_CERTIFICATE_PROFILE_NAME',
    ],
    none: 'the Windows installer is not Authenticode signed and SmartScreen warns on first run',
  },
  {
    id: 'sparkle',
    names: ['SPARKLE_ED_PRIVATE_KEY'],
    none: 'the native macOS apps get no appcast entry, so installed Macs are not offered this release',
  },
]

export const ALL_SECRET_NAMES = SECRET_GROUPS.flatMap((group) => group.names)

// `present` maps a secret name to whether it is set. Returns the errors (the
// run must stop), the warnings, and which signing each platform gets.
export function checkSecrets(present) {
  const errors = []
  const warnings = []
  const complete = {}
  for (const group of SECRET_GROUPS) {
    const set = group.names.filter((name) => present[name] === true)
    complete[group.id] = set.length === group.names.length
    if (complete[group.id]) continue
    if (group.required) {
      errors.push(`${group.names.join(', ')} must be set: it ${group.purpose}.`)
    } else if (set.length > 0) {
      const missing = group.names.filter((name) => !set.includes(name))
      errors.push(
        `Only ${set.length} of the ${group.names.length} ${group.id} secrets are set (missing ${missing.join(', ')}). Set all of them or none.`,
      )
    } else {
      warnings.push(`No ${group.id} secrets: ${group.none}.`)
    }
  }
  if (complete.sparkle && !complete.apple) {
    warnings.push(
      'SPARKLE_ED_PRIVATE_KEY is set but the Apple secrets are not: only notarized apps are offered through the appcast, so it goes unused.',
    )
  }
  return {
    errors,
    warnings,
    macSigning: complete.apple,
    sparkle: complete.apple && complete.sparkle,
    windowsSigning: complete.azure,
  }
}

// ---------------------------------------------------------------------------
// Reading what users read.
//
// The checks that matter are asked the way the shipped updaters ask them: with
// no credential at all, against github.com rather than api.github.com. fetch is
// a parameter so the tests can read back exactly what was sent: the point of
// this function is the headers it does NOT carry, and an ambient GH_TOKEN
// picked up by a helper somewhere is the failure it exists to rule out.
const ANONYMOUS_USER_AGENT = 'fairspoken-release'

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
