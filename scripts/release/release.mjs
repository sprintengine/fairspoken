#!/usr/bin/env node
// The GitHub-facing half of .github/workflows/release.yml. One file, one
// subcommand per workflow step:
//
//   resolve                        decide what this run builds: channel, version, commit, build number
//   check-secrets                  fail before anything is built when a secret group is partial
//   stamp <version>                write the version into package.json and Cargo.toml
//   collect <platform> <out-dir>   rename one desktop leg's bundles, check their signatures,
//                                  and write its share of desktop-manifest.json
//   sign-host <target> <dir>       sign one host archive with the Tauri key and write its
//                                  share of host-manifest.json
//   check-sparkle <dir>            check the Sparkle metadata release.sh wrote against the
//                                  zips it describes and the public key the apps ship
//   merge-manifests <dir>          fold every leg's share into desktop-manifest.json and
//                                  host-manifest.json
//   notes <out-file>               write the release body
//   verify-release                 prove the published release is complete (API, with the token)
//   update-feeds <out-dir>         render every update feed from the published releases and
//                                  upload them to the update-feeds release
//   verify-public <dir>            read the feeds (and the release) back with no credentials
//   prune-nightlies                delete nightly releases past the newest few
//   feeds-dry-run [--live] [--out <dir>]
//                                  render every feed into a directory and upload nothing:
//                                  from fixture releases, or (--live) from the real repository
//
// Inputs arrive as arguments and environment variables set by the workflow,
// outputs go to $GITHUB_OUTPUT. The pure logic is in release-lib.mjs and
// feeds.mjs.

import { execFileSync } from 'node:child_process'
import { createHash } from 'node:crypto'
import { appendFileSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import path from 'node:path'
import process from 'node:process'
import { fileURLToPath } from 'node:url'

import { feedsCoverRelease, FEEDS_RELEASE_BODY, FEEDS_RELEASE_TITLE, FEEDS_TAG, feedUrl, planFeeds, renderFeeds, verifyPublicFeeds } from './feeds.mjs'
import { isOnMain, resolveNightly, resolvePromotion, resolveTagRelease } from './main-release.mjs'
import { ed25519Problem, minisignProblem } from './minisign.mjs'
import { failedAttemptGate, lastNightly, nightlyGate } from './nightly-gate.mjs'
import {
  ALL_SECRET_NAMES,
  anonymousGet,
  buildNumber,
  buildReleaseNotes,
  channelForVersion,
  checkDesktopManifest,
  checkHostManifest,
  checkSecrets,
  checkSparkleMetadata,
  compareCore,
  DESKTOP_MANIFEST,
  desktopFragment,
  desktopFragmentName,
  FRAGMENT_RE,
  HOST_MANIFEST,
  hostArchiveLayout,
  hostFragment,
  hostFragmentName,
  macAppNameFromXcconfig,
  macAppsWithoutSparkle,
  mergeFragments,
  missingInstallers,
  NIGHTLIES_KEPT,
  nightliesToPrune,
  parseVersion,
  planCollect,
  productSlug,
  releasePageUrl,
  releasesRepoFromEndpoint,
  sourceShaFromBody,
  SPARKLE_APPS,
  tagChannel,
  unsignedMacApps,
  utcDateStamp,
} from './release-lib.mjs'

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..')
const packageJsonPath = path.join(repoRoot, 'package.json')
const cargoTomlPath = path.join(repoRoot, 'src-tauri', 'Cargo.toml')
const packageJson = JSON.parse(readFileSync(packageJsonPath, 'utf8'))
const tauriConfig = JSON.parse(readFileSync(path.join(repoRoot, 'src-tauri', 'tauri.conf.json'), 'utf8'))
const PRODUCT_NAME = tauriConfig.productName
const SLUG = productSlug(PRODUCT_NAME)
// The public half of the Tauri updater key: every desktop payload and host
// archive is checked against it before it is published.
const UPDATER_PUBKEY = tauriConfig.plugins?.updater?.pubkey ?? ''
// The releases repository is whatever the shipped updater asks, so the workflow
// and installed builds cannot disagree about it. Read on first use, so the
// commands that never touch GitHub (stamp, the fixture dry-run) do not need it.
let releasesRepo = null
const repoOfEndpoint = () => (releasesRepo ??= releasesRepoFromEndpoint(tauriConfig.plugins?.updater?.endpoints?.[0]))
// The native macOS app's name, from the one place it is defined; the server
// app's files are named after it too (macServerName). Null on a commit from
// before apps/macos existed, which then ships no native app.
const MAC_APP_NAME = (() => {
  try {
    return macAppNameFromXcconfig(readFileSync(path.join(repoRoot, 'apps', 'macos', 'Config', 'Base.xcconfig'), 'utf8'))
  } catch {
    return null
  }
})()

function env(name, { required = true } = {}) {
  const value = process.env[name] ?? ''
  if (required && value === '') throw new Error(`${name} is not set`)
  return value
}

function setOutputs(outputs) {
  const lines = Object.entries(outputs).map(([key, value]) => `${key}=${value}`)
  for (const line of lines) console.log(line)
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `${lines.join('\n')}\n`)
}

const warn = (message) => console.log(process.env.GITHUB_ACTIONS ? `::warning::${message}` : `warning: ${message}`)

const API_HEADERS = { 'user-agent': 'fairspoken-release', 'x-github-api-version': '2022-11-28' }

async function request(method, url, token, { accept = 'application/vnd.github+json', body, contentType, allow404 = false } = {}) {
  const headers = { ...API_HEADERS, accept }
  if (token) headers.authorization = `Bearer ${token}`
  if (contentType) headers['content-type'] = contentType
  const response = await fetch(url.startsWith('http') ? url : `https://api.github.com${url}`, { method, headers, body })
  if (allow404 && response.status === 404) return null
  if (!response.ok) throw new Error(`${method} ${url} answered ${response.status}: ${await response.text()}`)
  if (response.status === 204) return null
  return accept.includes('json') ? response.json() : response.text()
}

const github = (apiPath, token, options = {}) => request('GET', apiPath, token, options)

async function listReleases(token) {
  const releases = []
  for (let page = 1; ; page += 1) {
    const batch = await github(`/repos/${repoOfEndpoint()}/releases?per_page=100&page=${page}`, token)
    releases.push(...batch)
    if (batch.length < 100) break
  }
  return releases.filter((release) => !release.draft && release.published_at)
}

function versionOf(release) {
  try {
    return { version: parseVersion(release.tag_name), raw: release.tag_name.replace(/^v/, '') }
  } catch {
    return null
  }
}

function latestStable(releases) {
  return releases
    .map(versionOf)
    .filter((entry) => entry && entry.version.pre === null)
    .map((entry) => entry.raw)
    .sort((a, b) => compareCore(b, a))[0] ?? null
}

function latestRelease(releases, predicate) {
  return releases
    .filter((release) => {
      const entry = versionOf(release)
      return entry && predicate(entry.raw)
    })
    .sort((a, b) => Date.parse(b.published_at) - Date.parse(a.published_at))[0] ?? null
}

// Every entry point, and what it builds:
//
//   push of a tag vX.Y.Z      that commit, as stable vX.Y.Z (the hotfix route)
//   schedule                  main's head as a nightly, when the gate allows
//   dispatch nightly          main's head as a nightly, gate skipped
//   dispatch stable           the commit the latest nightly shipped, as stable
//   dispatch feeds            nothing: re-render the update feeds only
//
// A push to main is not an entry point: merging publishes nothing.
async function resolve() {
  const eventName = env('EVENT_NAME')
  const sha = env('SHA')
  const sourceRepo = env('SOURCE_REPO')
  const sourceToken = env('SOURCE_TOKEN')
  const dispatchChannel = env('DISPATCH_CHANNEL', { required: false }) || 'nightly'
  const publish = eventName !== 'workflow_dispatch' || env('DISPATCH_PUBLISH', { required: false }) !== 'false'
  const now = new Date()

  if (repoOfEndpoint() !== sourceRepo) {
    throw new Error(`The updater endpoint names ${repoOfEndpoint()}, but this is ${sourceRepo}. Releases publish to the source repository.`)
  }
  if (eventName === 'workflow_dispatch' && (env('REF_TYPE') !== 'branch' || env('REF_NAME') !== 'main')) {
    throw new Error(`Run release dispatches from main, not ${env('REF_NAME')}.`)
  }
  if (eventName === 'workflow_dispatch' && dispatchChannel === 'feeds') {
    console.log('Re-rendering the update feeds only; nothing is built.')
    setOutputs({ should_build: 'false', feeds_only: 'true', publish: 'false', ref: sha })
    return
  }

  const source = await github(`/repos/${sourceRepo}`, sourceToken)
  const releases = await listReleases(sourceToken)
  const stable = latestStable(releases)
  const lastStableRelease = latestRelease(releases, (raw) => tagChannel(raw) === 'stable')
  const date = utcDateStamp(now.toISOString())

  let version
  let ref = sha
  let shouldBuild = true
  let previous = null

  if (eventName === 'push') {
    // A pushed tag builds exactly that commit as that stable (the hotfix
    // route). package.json is not consulted: it stays at the development
    // baseline, and the build stamps the tag's version.
    if (env('REF_TYPE') !== 'tag') throw new Error('A push to a branch publishes nothing. Release from a tag or a dispatch.')
    version = resolveTagRelease({
      refName: env('REF_NAME'),
      latestStable: stable,
      publishedTags: releases.map((release) => release.tag_name),
    })
    previous = lastStableRelease
  } else if (eventName === 'schedule' || dispatchChannel === 'nightly') {
    const last = lastNightly(releases)
    previous = last
    if (eventName === 'schedule') {
      const comparison = await compareWithMain(sourceRepo, sourceToken, last, sha)
      // Throws when main was rewritten under the last nightly: the run fails
      // and says so rather than skipping every tick without a word.
      const gate = nightlyGate({ releases, comparison, now })
      console.log(gate.reason)
      shouldBuild = gate.publish
      if (shouldBuild) {
        const runs = await github(`/repos/${sourceRepo}/actions/runs?event=schedule&head_sha=${sha}&status=failure&per_page=50`, sourceToken)
        const retry = failedAttemptGate({
          runs: (runs?.workflow_runs ?? []).filter((run) => run.path === '.github/workflows/release.yml'),
          sha,
          now,
          currentRunId: Number(env('RUN_ID', { required: false })) || null,
        })
        if (!retry.publish) {
          console.log(retry.reason)
          shouldBuild = false
        }
      }
    }
    if (shouldBuild) {
      const plan = resolveNightly({
        sha,
        releases,
        packageVersion: packageJson.version,
        date,
        runNumber: env('RUN_NUMBER'),
        cwd: repoRoot,
      })
      if (!plan.shouldBuild) {
        const message = `${sha} is already shipped by stable v${plan.base}; a nightly of it would sort below that stable.`
        if (eventName !== 'schedule') throw new Error(message)
        console.log(`${message} Skipping.`)
        shouldBuild = false
      }
      version = plan.version ?? plan.base
    } else {
      version = last?.tag_name.replace(/^v/, '') ?? packageJson.version
    }
    if (shouldBuild) console.log(`Cutting a nightly of main ${sha} as ${version}.`)
  } else if (dispatchChannel === 'stable') {
    // Stable ships the exact commit the latest nightly shipped, so a stable
    // build is always one nightly users have already run, and merges that land
    // while a maintainer checks that nightly never reach it.
    const nightly = lastNightly(releases)
    if (!nightly) throw new Error('No published nightly to promote. Dispatch a nightly first.')
    ref = sourceShaFromBody(nightly.body)
    if (!ref) throw new Error(`${nightly.tag_name} does not record its source commit, so it cannot be promoted.`)
    if (!isOnMain({ sha: ref, mainSha: sha, cwd: repoRoot })) {
      throw new Error(`${nightly.tag_name} shipped ${ref}, which is not on main. Cut a new nightly and promote that.`)
    }
    version = resolvePromotion({
      nightlyTag: nightly.tag_name,
      override: env('DISPATCH_VERSION', { required: false }),
      latestStable: stable,
      tags: gitTags(),
    })
    previous = lastStableRelease
    console.log(`Promoting ${nightly.tag_name} (${ref}) to ${version}.`)
  } else {
    throw new Error(`Unknown release channel ${dispatchChannel}`)
  }

  const channel = channelForVersion(version)
  const tag = `v${version}`
  if (shouldBuild && channel === 'stable' && stable && compareCore(version, stable) <= 0) {
    throw new Error(`${tag} cannot replace the newer or equal stable v${stable}.`)
  }
  if (shouldBuild && releases.some((release) => release.tag_name === tag)) {
    throw new Error(`${tag} is already published on ${repoOfEndpoint()}.`)
  }

  setOutputs({
    should_build: String(shouldBuild),
    feeds_only: 'false',
    publish: String(publish),
    version,
    tag,
    channel,
    prerelease: String(channel !== 'stable'),
    ref,
    // One build number for every product of this release (contract §1).
    build_number: buildNumber(now.toISOString()),
    previous_sha: sourceShaFromBody(previous?.body) ?? '',
    source_private: String(source.private),
    slug: SLUG,
  })
}

function gitTags() {
  return execFileSync('git', ['tag', '--list', 'v*'], { cwd: repoRoot, encoding: 'utf8' }).split('\n').filter(Boolean)
}

// The comparison the nightly gate reads: the commit the last nightly shipped
// against main's head. Null when there is nothing to compare against.
async function compareWithMain(sourceRepo, token, last, sha) {
  const shipped = sourceShaFromBody(last?.body)
  if (!shipped) return null
  if (shipped === sha) return { status: 'identical' }
  return github(`/repos/${sourceRepo}/compare/${shipped}...${sha}?per_page=1`, token, { allow404: true })
}

// Runs before any build. The workflow passes HAS_<SECRET>=true|false for every
// secret, never a value.
function checkSecretsCommand() {
  const present = Object.fromEntries(ALL_SECRET_NAMES.map((name) => [name, process.env[`HAS_${name}`] === 'true']))
  const result = checkSecrets(present)
  for (const warning of result.warnings) warn(warning)
  setOutputs({
    mac_signing: String(result.macSigning),
    sparkle: String(result.sparkle),
    windows_signing: String(result.windowsSigning),
  })
  if (result.errors.length > 0) throw new Error(`Secrets are misconfigured; nothing was built:\n  - ${result.errors.join('\n  - ')}`)
}

// A nightly or promoted version is not what package.json says at this commit,
// and it must be the same in the bundle, the app's own Settings screen and the
// updater manifest. tauri.conf.json reads its version from package.json; the
// Cargo version is what the bundled transcription host reports.
function stamp(version) {
  parseVersion(version)
  packageJson.version = version
  writeFileSync(packageJsonPath, `${JSON.stringify(packageJson, null, 2)}\n`)

  const cargo = readFileSync(cargoTomlPath, 'utf8')
  const packageEnd = cargo.indexOf('\n[', cargo.indexOf('[package]') + 1)
  const head = cargo.slice(0, packageEnd)
  if (!/^version = "[^"]*"$/m.test(head)) throw new Error('No version line in the [package] table of Cargo.toml')
  writeFileSync(cargoTomlPath, head.replace(/^version = "[^"]*"$/m, `version = "${version}"`) + cargo.slice(packageEnd))
  console.log(`Stamped ${version} into package.json and src-tauri/Cargo.toml.`)
}

function listFiles(dir) {
  return readdirSync(dir, { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile())
    .map((entry) => path.join(entry.parentPath, entry.name))
}

// A signature that does not verify against the key the apps ship would be
// refused by every installed build: fail here, before anything is published.
function assertSignedForShippedKey(file, signature) {
  const problem = minisignProblem({ data: readFileSync(file), signature, publicKey: UPDATER_PUBKEY })
  if (problem) {
    throw new Error(
      `${path.basename(file)}: ${problem}. TAURI_SIGNING_PRIVATE_KEY does not match plugins.updater.pubkey in tauri.conf.json.`,
    )
  }
}

function collect(platform, outDir) {
  const version = env('VERSION')
  const bundleDir = env('BUNDLE_DIR')
  const plan = planCollect({ files: listFiles(bundleDir), platform, slug: SLUG, version })
  mkdirSync(outDir, { recursive: true })
  const payloads = plan.payloads.map((payload) => {
    const signature = readFileSync(payload.signature, 'utf8')
    assertSignedForShippedKey(payload.file, signature)
    return { ...payload, signature }
  })
  for (const { from, to } of plan.copies) {
    copyFileSync(from, path.join(outDir, to))
    console.log(`${path.relative(bundleDir, from)} -> ${to}`)
  }
  const fragment = desktopFragment({ version, repo: repoOfEndpoint(), tag: env('TAG'), payloads })
  writeFileSync(path.join(outDir, desktopFragmentName(platform)), `${JSON.stringify(fragment, null, 2)}\n`)
}

// Signs one host archive with the same Tauri key as the desktop app (contract
// §3), checks the signature against the shipped public key, and writes this
// leg's share of host-manifest.json. The .sig is folded into the manifest and
// not uploaded on its own.
function signHost(target, dir) {
  const version = env('VERSION')
  const layout = hostArchiveLayout({ slug: SLUG, version, target })
  const archive = path.join(dir, layout.archive)
  if (!existsSync(archive)) throw new Error(`No host archive ${archive}`)
  execFileSync('npx', ['tauri', 'signer', 'sign', archive], {
    cwd: repoRoot,
    stdio: ['ignore', 'ignore', 'inherit'],
    shell: process.platform === 'win32',
  })
  const signatureFile = `${archive}.sig`
  const signature = readFileSync(signatureFile, 'utf8')
  assertSignedForShippedKey(archive, signature)
  const sha256 = createHash('sha256').update(readFileSync(archive)).digest('hex')
  const fragment = hostFragment({ slug: SLUG, version, repo: repoOfEndpoint(), tag: env('TAG'), target, sha256, signature })
  writeFileSync(path.join(dir, hostFragmentName(target)), `${JSON.stringify(fragment, null, 2)}\n`)
  rmSync(signatureFile)
  console.log(`${layout.archive}: sha256 ${sha256}, signed; the binary is ${layout.binary}.`)
}

// The Sparkle metadata release.sh wrote, checked before it leaves the Mac
// runner: the fields, the zip's length, and the EdDSA signature against the
// SUPublicEDKey the app itself carries, so a SPARKLE_ED_PRIVATE_KEY that does
// not match the shipped public key fails the build rather than every update.
function checkSparkle(dir) {
  const version = env('VERSION')
  const build = env('BUILD_NUMBER')
  const assets = readdirSync(dir).map((name) => ({ name, size: statSync(path.join(dir, name)).size }))
  const problems = []
  for (const app of SPARKLE_APPS) {
    const file = path.join(dir, app.metadata)
    if (!existsSync(file)) {
      warn(`${app.title} has no ${app.metadata}: this release will not be in ${app.appcast}.`)
      continue
    }
    const meta = JSON.parse(readFileSync(file, 'utf8'))
    const found = checkSparkleMetadata(meta, { app, version, assets })
    if (meta.version !== build) found.push(`has build ${meta.version}, but this release is build ${build}`)
    if (found.length === 0) {
      const problem = ed25519Problem({ data: readFileSync(path.join(dir, meta.file)), signature: meta.edSignature, publicKey: meta.publicEDKey })
      if (problem) found.push(`${problem}: SPARKLE_ED_PRIVATE_KEY does not match SUPublicEDKey (${meta.publicEDKey}) in the app`)
    }
    problems.push(...found.map((problem) => `${app.metadata} ${problem}`))
    if (found.length === 0) console.log(`${app.metadata}: ${meta.file}, build ${meta.version}, signature verified against ${meta.publicEDKey}.`)
  }
  if (problems.length > 0) throw new Error(`The Sparkle metadata is not usable:\n  - ${problems.join('\n  - ')}`)
}

function mergeManifests(dir) {
  const fragments = (kind) =>
    readdirSync(dir)
      .filter((name) => FRAGMENT_RE.test(name) && name.startsWith(`fragment-${kind}-`))
      .map((name) => ({ name, doc: JSON.parse(readFileSync(path.join(dir, name), 'utf8')) }))
  for (const [kind, output] of [
    ['desktop', DESKTOP_MANIFEST],
    ['host', HOST_MANIFEST],
  ]) {
    const found = fragments(kind)
    const version = found[0]?.doc.version
    const merged = mergeFragments(
      found.map((fragment) => fragment.doc),
      { notes: version ? releasePageUrl({ repo: repoOfEndpoint(), tag: `v${version}` }) : '', pub_date: new Date().toISOString() },
    )
    writeFileSync(path.join(dir, output), `${JSON.stringify(merged, null, 2)}\n`)
    for (const { name } of found) rmSync(path.join(dir, name))
    console.log(`${output}:\n${JSON.stringify(merged, null, 2)}`)
  }
}

async function notes(outFile) {
  const sourceRepo = env('SOURCE_REPO')
  const sourcePrivate = env('SOURCE_PRIVATE') === 'true'
  const sha = env('REF')
  const previousSha = env('PREVIOUS_SHA', { required: false })
  let commits = []
  if (!sourcePrivate && previousSha) {
    const comparison = await github(`/repos/${sourceRepo}/compare/${previousSha}...${sha}`, env('SOURCE_TOKEN'), { allow404: true })
    commits = (comparison?.commits ?? [])
      .map((commit) => ({ sha: commit.sha, subject: commit.commit.message.split('\n')[0] }))
      .reverse()
  }
  writeFileSync(
    outFile,
    buildReleaseNotes({ productName: PRODUCT_NAME, version: env('VERSION'), channel: env('CHANNEL'), sourceRepo, sha, sourcePrivate, commits }),
  )
}

const downloadAsset = (asset, token) =>
  github(`/repos/${repoOfEndpoint()}/releases/assets/${asset.id}`, token, { accept: 'application/octet-stream' })

// A release that publishes to the wrong place, publishes half its files, or
// ships metadata the feeds cannot use must not report success. This is the
// pass with the job's token; verify-public is the one without.
async function verifyRelease() {
  const token = env('SOURCE_TOKEN')
  const tag = env('TAG')
  const version = env('VERSION')
  const channel = env('CHANNEL')
  const release = await github(`/repos/${repoOfEndpoint()}/releases/tags/${tag}`, token, { allow404: true })
  if (!release) throw new Error(`No release ${tag} on ${repoOfEndpoint()}.`)

  const problems = []
  if (release.draft) problems.push('the release is still a draft')
  if (release.prerelease !== (channel !== 'stable')) problems.push(`prerelease is ${release.prerelease}`)
  const assetNames = release.assets.map((asset) => asset.name)
  console.log(`Assets on ${tag}:\n${assetNames.map((name) => `  ${name}`).join('\n')}`)
  const products = { slug: SLUG, version, macAppName: MAC_APP_NAME }
  for (const missing of missingInstallers(assetNames, products)) problems.push(`missing ${missing}`)
  for (const name of unsignedMacApps(assetNames, products)) {
    warn(`${name} is ad-hoc signed: set the Apple signing secrets to sign and notarize the native macOS apps.`)
  }
  for (const title of macAppsWithoutSparkle(assetNames, products)) {
    warn(
      `${title} is notarized but carries no Sparkle metadata, so its appcast will not offer this release. ` +
        'Is SPARKLE_ED_PRIVATE_KEY set, and SUPublicEDKey in the app?',
    )
  }
  if (assetNames.some((name) => FRAGMENT_RE.test(name))) problems.push('the manifest fragments were not merged')

  const read = async (name) => {
    const asset = release.assets.find((candidate) => candidate.name === name)
    return asset ? downloadAsset(asset, token) : null
  }
  for (const [name, check] of [
    [DESKTOP_MANIFEST, checkDesktopManifest],
    [HOST_MANIFEST, checkHostManifest],
  ]) {
    const text = await read(name)
    if (text !== null) for (const problem of check(text, { version, repo: repoOfEndpoint(), tag, assetNames })) problems.push(`${name} ${problem}`)
  }
  for (const app of SPARKLE_APPS) {
    const text = await read(app.metadata)
    if (text !== null) {
      for (const problem of checkSparkleMetadata(text, { app, version, assets: release.assets })) problems.push(`${app.metadata} ${problem}`)
    }
  }

  if (problems.length > 0) throw new Error(`Release ${tag} is not installable:\n  - ${problems.join('\n  - ')}`)
  console.log(`Release ${tag} is complete on ${repoOfEndpoint()}.`)
}

// The rolling release the feeds live on: prerelease, never latest, created on
// first use. Its tag is not a version, so nothing that lists releases by
// version (the nightly gate, promotion, retention) ever picks it up.
async function ensureFeedsRelease(token, targetSha) {
  const existing = await github(`/repos/${repoOfEndpoint()}/releases/tags/${FEEDS_TAG}`, token, { allow404: true })
  if (existing) {
    if (!existing.prerelease) {
      return request('PATCH', `/repos/${repoOfEndpoint()}/releases/${existing.id}`, token, {
        body: JSON.stringify({ prerelease: true, make_latest: 'false' }),
        contentType: 'application/json',
      })
    }
    return existing
  }
  console.log(`Creating the ${FEEDS_TAG} release.`)
  return request('POST', `/repos/${repoOfEndpoint()}/releases`, token, {
    body: JSON.stringify({
      tag_name: FEEDS_TAG,
      target_commitish: targetSha,
      name: FEEDS_RELEASE_TITLE,
      body: FEEDS_RELEASE_BODY,
      draft: false,
      prerelease: true,
      make_latest: 'false',
    }),
    contentType: 'application/json',
  })
}

async function listAssets(token, releaseId) {
  const assets = []
  for (let page = 1; ; page += 1) {
    const batch = await github(`/repos/${repoOfEndpoint()}/releases/${releaseId}/assets?per_page=100&page=${page}`, token)
    assets.push(...batch)
    if (batch.length < 100) break
  }
  return assets
}

const deleteAsset = (token, id) => request('DELETE', `/repos/${repoOfEndpoint()}/releases/assets/${id}`, token, { allow404: true })

// Replaces one feed asset with as short a gap as GitHub allows: the new copy
// is uploaded under a temporary name, then the old one deleted and the new one
// renamed into its place, so a reader sees the old feed or the new one and at
// worst a sub-second 404, never a half-uploaded file. A temporary copy left by
// a run that died midway is cleared here too.
async function replaceAsset(token, release, name, text) {
  const temporary = `${name}.uploading-${Date.now()}`
  const uploadUrl = release.upload_url.replace(/\{.*\}$/, '')
  const contentType = name.endsWith('.xml') ? 'application/xml' : 'application/json'
  const uploaded = await request('POST', `${uploadUrl}?name=${encodeURIComponent(temporary)}`, token, {
    body: Buffer.from(text, 'utf8'),
    contentType,
  })
  for (const asset of await listAssets(token, release.id)) {
    if (asset.id !== uploaded.id && (asset.name === name || asset.name.includes('.uploading-'))) await deleteAsset(token, asset.id)
  }
  await request('PATCH', `/repos/${repoOfEndpoint()}/releases/assets/${uploaded.id}`, token, {
    body: JSON.stringify({ name }),
    contentType: 'application/json',
  })
}

function logRender({ plan, rendered }) {
  if (plan.pruned.length > 0) console.log(`Left out (about to be pruned): ${plan.pruned.join(', ')}`)
  for (const [name, tags] of Object.entries(rendered.sources)) console.log(`${name} <- ${tags.join(', ') || '(no items)'}`)
  for (const warning of rendered.warnings) warn(warning)
}

// Renders every feed from the releases published right now and uploads them.
// Runs in its own serialized job (release.yml `feeds`), so two publishes
// never interleave their uploads, and the later one always renders from a list
// that includes the earlier one's release.
async function updateFeeds(outDir) {
  const token = env('GH_TOKEN')
  const releases = await listReleases(token)
  // The release this run just published, in case the list lags it.
  const tag = env('TAG', { required: false })
  if (tag && !releases.some((release) => release.tag_name === tag)) {
    const fresh = await github(`/repos/${repoOfEndpoint()}/releases/tags/${tag}`, token, { allow404: true })
    if (fresh) releases.push(fresh)
  }
  const plan = planFeeds(releases)
  const cache = new Map()
  const rendered = await renderFeeds(plan, {
    repo: repoOfEndpoint(),
    read: async (release, name) => {
      const asset = release.assets.find((candidate) => candidate.name === name)
      if (!cache.has(asset.id)) cache.set(asset.id, await downloadAsset(asset, token))
      return cache.get(asset.id)
    },
  })
  logRender({ plan, rendered })
  mkdirSync(outDir, { recursive: true })
  for (const [name, text] of Object.entries(rendered.files)) writeFileSync(path.join(outDir, name), text)

  if (tag) {
    const release = releases.find((candidate) => candidate.tag_name === tag)
    const sparkleApps = SPARKLE_APPS.filter((app) => release?.assets.some((asset) => asset.name === app.metadata))
    const problems = feedsCoverRelease(rendered.files, { version: env('VERSION'), channel: env('CHANNEL'), sparkleApps })
    if (problems.length > 0) throw new Error(`The feeds do not carry ${tag}; nothing was uploaded:\n  - ${problems.join('\n  - ')}`)
  }

  const feedsRelease = await ensureFeedsRelease(token, env('REF'))
  for (const [name, text] of Object.entries(rendered.files)) {
    await replaceAsset(token, feedsRelease, name, text)
    console.log(`Uploaded ${feedUrl({ repo: repoOfEndpoint(), name })}`)
  }
}

// The same feeds, read back the way installed apps read them: no credentials,
// from github.com. For a release run, also that /releases/latest is a stable.
async function verifyPublic(dir) {
  const files = Object.fromEntries(readdirSync(dir).map((name) => [name, readFileSync(path.join(dir, name), 'utf8')]))
  const tag = env('TAG', { required: false })
  const release = tag ? { tag, channel: env('CHANNEL') } : null
  const problems = await verifyPublicFeeds({ repo: repoOfEndpoint(), files, release, get: anonymousGet() })
  if (problems.length > 0) throw new Error(`Installed apps cannot read the update feeds:\n  - ${problems.join('\n  - ')}`)
  console.log(`The update feeds on ${repoOfEndpoint()} are readable without credentials.`)
}

// Deletes the nightly releases past the newest few, never their tags: a tag is
// how a version stays in the history and in the numbering, and it costs
// nothing to keep. Runs after the feeds were re-rendered without them.
async function pruneNightlies() {
  const token = env('GH_TOKEN')
  const releases = await listReleases(token)
  const tags = nightliesToPrune(releases)
  if (tags.length === 0) {
    console.log(`No nightly past the newest ${NIGHTLIES_KEPT} to delete.`)
    return
  }
  for (const tag of tags) {
    const { id } = releases.find((release) => release.tag_name === tag)
    // Already gone: another run pruned it first.
    await request('DELETE', `/repos/${repoOfEndpoint()}/releases/${id}`, token, { allow404: true })
    console.log(`Deleted the ${tag} release; its tag stays.`)
  }
}

// Renders every feed into a directory and uploads nothing. By default from the
// fixture releases in fixtures/releases.mjs (what the tests use); with --live
// from the real repository (GH_TOKEN optional once it is public).
async function feedsDryRun(args) {
  const live = args.includes('--live')
  const outIndex = args.indexOf('--out')
  const outDir = outIndex >= 0 ? path.resolve(args[outIndex + 1]) : mkdtempSync(path.join(tmpdir(), 'fairspoken-feeds-'))
  let releases
  let read
  let repo
  if (live) {
    repo = repoOfEndpoint()
    const token = process.env.GH_TOKEN ?? ''
    releases = await listReleases(token)
    read = async (release, name) => downloadAsset(release.assets.find((asset) => asset.name === name), token)
  } else {
    const fixtures = await import('./fixtures/releases.mjs')
    repo = fixtures.FIXTURE_REPO
    releases = fixtures.fixtureReleases()
    read = async (release, name) => release.assets.find((asset) => asset.name === name).content
  }
  const plan = planFeeds(releases)
  const rendered = await renderFeeds(plan, { repo, read })
  logRender({ plan, rendered })
  mkdirSync(outDir, { recursive: true })
  for (const [name, text] of Object.entries(rendered.files)) writeFileSync(path.join(outDir, name), text)
  console.log(`\n${Object.keys(rendered.files).length} feeds for ${repo} written to ${outDir} (nothing uploaded):`)
  for (const name of Object.keys(rendered.files)) console.log(`  ${path.join(outDir, name)}  -> ${feedUrl({ repo, name })}`)
}

const [command, ...args] = process.argv.slice(2)
const commands = {
  resolve: () => resolve(),
  'check-secrets': () => checkSecretsCommand(),
  stamp: () => stamp(args[0]),
  collect: () => collect(args[0], args[1]),
  'sign-host': () => signHost(args[0], args[1]),
  'check-sparkle': () => checkSparkle(args[0]),
  'merge-manifests': () => mergeManifests(args[0]),
  notes: () => notes(args[0] ?? 'release-notes.md'),
  'verify-release': () => verifyRelease(),
  'update-feeds': () => updateFeeds(args[0] ?? 'update-feeds'),
  'verify-public': () => verifyPublic(args[0] ?? 'update-feeds'),
  'prune-nightlies': () => pruneNightlies(),
  'feeds-dry-run': () => feedsDryRun(args),
}

if (!commands[command]) {
  console.error(`usage: release.mjs ${Object.keys(commands).join(' | ')}`)
  process.exit(2)
}
try {
  await commands[command]()
} catch (error) {
  console.error(error.message)
  process.exit(1)
}
