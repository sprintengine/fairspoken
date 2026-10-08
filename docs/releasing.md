# Releasing

Fairspoken ships on two trains, stable and nightly, from
`.github/workflows/release.yml`. Every installed app (the Tauri desktop app,
the two native Mac apps and the standalone transcription host) updates itself
from feeds on one rolling GitHub release of this repository. The release rules
live in `scripts/release/` and are covered by `npm run test:release`.

One train carries every product. Every nightly and every stable is one GitHub
release, built from one commit at one version, holding:

- the **Tauri desktop app** (`src-tauri/`, `src/`) for Windows and Linux. It is
  not released for macOS: Macs use the native apps below;
- the standalone **transcription host** (`transcription-host`, a bin of the same
  crate) for headless machines, on macOS (arm64 and x64), Windows and Linux;
- the two **native macOS apps** (`apps/macos/`): **Fairspoken**, the dictation
  client (`ie.fairspoken.mac`), and **Fairspoken Server**, a transcription
  server with its own window (`ie.fairspoken.server`).

They share the train because they share the code and the wire protocol
(`src-tauri/src/host/PROTOCOL.md`): a host and the clients of the same release
are always a matched set, and one nightly gate, one promotion and one tag cover
them all.

## Channels, versions and build numbers

**Nightly** is main as it stands, a few times a day: versions
`X.Y.Z-nightly.YYYYMMDD.RUN`, published as GitHub prereleases that are never
marked latest. **Stable** is `X.Y.Z`, tagged `vX.Y.Z` and marked latest, and is
always a build of a commit some nightly already shipped (the hotfix tag below
is the one exception).

`X.Y.Z` in a nightly is the version the next stable would take: the strongest
Conventional Commit since the last stable tag, applied to that tag (`feat` is a
minor, everything else a patch; before 1.0 a breaking change is a minor too, and
1.0.0 is only ever typed in). So a nightly is always newer than the stable
before it. A version a tag already holds is never reused. `RUN` is the
workflow's run number; it only orders nightlies of the same day against each
other.

Every product of one release shares one **build number**: the UTC minute the
release was resolved, `YYYYMMDDHHMM` (`buildNumber` in
`scripts/release/release-lib.mjs`). It is the Mac apps' `CFBundleVersion` and so
what Sparkle compares. It only grows, whatever repository or run counter a build
came from, and a stable promoted from a nightly is resolved after it, so it is
always above that nightly. Nothing derives a build number from
`github.run_number`.

A build's default channel is derived from its own version (`-nightly.` means
nightly, anything else stable); a channel the user picks in Settings overrides
it.

## The update feeds

All feeds are assets of one release: tag `update-feeds`, title "Update feeds",
a prerelease that is never latest, created by the first publish that needs it
and never deleted by nightly retention (its tag is not a version, so nothing
that lists releases by version ever picks it up). Base URL:

```
https://github.com/sprintengine/fairspoken/releases/download/update-feeds/
```

| Asset | Read by | Contents |
| --- | --- | --- |
| `desktop-stable.json`, `desktop-nightly.json` | the Tauri desktop app | Tauri's static updater JSON: `version`, `notes` (the release page), `pub_date`, `platforms` with `windows-x86_64` (NSIS installer), `linux-x86_64` (AppImage) and `linux-x86_64-deb` (.deb), each `{ url, signature }` |
| `appcast-fairspoken.xml` | Fairspoken (Mac) | Sparkle 2 appcast, both channels |
| `appcast-fairspoken-server.xml` | Fairspoken Server (Mac) | Sparkle 2 appcast, both channels |
| `host-stable.json`, `host-nightly.json` | the transcription host | `version`, `channel`, `pub_date`, `notes`, `platforms` (`linux-x86_64`, `linux-aarch64`, `windows-x86_64`, `darwin-aarch64`; no Intel Mac build, since ort ships no prebuilt ONNX Runtime for it), each `{ url, sha256, signature, format }` |

Every URL inside a feed points at the asset on its own release
(`.../releases/download/vX.Y.Z[-nightly...]/<file>`), never at `update-feeds`.
The desktop endpoint in `tauri.conf.json` is `desktop-stable.json` on this
release; the app reads `desktop-nightly.json` beside it on the nightly channel.
The release scripts read the repository from that endpoint and refuse to
publish anywhere else.

**Feeds are rendered, never appended to.** After every publish the `feeds` job
lists the releases that exist, leaves out drafts, anything that is not a
version, and the nightlies retention is about to delete, and renders every feed
from the small metadata assets each release carries:

| Release asset | Written by | Feeds |
| --- | --- | --- |
| `desktop-manifest.json` | each desktop package leg (`collect`), merged in `publish` | `desktop-<channel>.json` from the newest release of that channel |
| `host-manifest.json` | each package leg (`sign-host`), merged in `publish` | `host-<channel>.json` from the newest release of that channel |
| `sparkle-fairspoken.json`, `sparkle-fairspoken-server.json` | `apps/macos/scripts/release.sh` | one appcast item each, from the newest 3 stable and newest 3 nightly releases that carry one |

"Newest" is the highest version for stable and the latest publish time for
nightly. A release whose metadata does not check out is skipped with a warning
(the next one is used), so a bad release can hold a feed back but never break
it; the publish that made it then fails its own check, because its release is
missing from the feeds. A JSON feed with no release to render from (no stable
published yet, or every one deleted) is not written, and a copy of it an
earlier publish left on `update-feeds` is deleted; an appcast is always
written, empty if need be.
Then the feeds are uploaded (each to a temporary name, the old copy deleted and
the new one renamed into place, so readers see the old or the new file), read
back without credentials, and only then are the old nightlies deleted. The job
has a queue of its own, so two publishes never interleave their uploads.

Sparkle items: stable items carry no channel element (Sparkle's default
channel, which every install sees); nightly items carry
`<sparkle:channel>nightly</sparkle:channel>`. Each has `sparkle:version` (the
build number), `sparkle:shortVersionString` (the full version),
`sparkle:minimumSystemVersion`, `pubDate`, a `sparkle:releaseNotesLink` to the
GitHub release, and an enclosure: the zip of the notarized, stapled app with
`length` and `sparkle:edSignature`.

To re-render the feeds without building anything (after deleting a bad
release by hand, or to create `update-feeds` on a new repository), dispatch the
workflow with channel `feeds`. To see what would be rendered, without uploading:

```sh
node scripts/release/release.mjs feeds-dry-run              # from fixture releases, into a temp dir
node scripts/release/release.mjs feeds-dry-run --live --out /tmp/feeds   # from the real releases
```

### Channel semantics in the apps

- Stable to nightly: the next check reads the nightly feed, which is newer, so
  it is offered.
- Nightly to stable, desktop and host: the app offers the latest stable even
  though it sorts lower ("Switch to stable X.Y.Z").
- Nightly to stable, Mac: Sparkle cannot downgrade. Because build numbers are
  timestamps, the first stable cut after the running nightly is offered
  automatically.
- Checks run about 30 seconds after launch, then every 6 hours, on demand, and
  when the channel changes.

## What a merge to main does

Nothing, until the next nightly. CI gates the pull request; the release
workflow does not listen for pushes to main.

## How a nightly is cut

The workflow wakes every 30 minutes (minutes 7 and 37). A scheduled run builds
only when all of these hold:

- at least six hours have passed since the last published nightly (none at
  all also counts);
- main has commits the last nightly did not ship;
- no scheduled nightly of the same commit failed in the last six hours. Without
  this a broken build (a missing secret, a red quality gate) would start the
  whole matrix, macOS legs included, every half hour. A new commit on main, or a
  dispatch, goes straight through.

Otherwise the run ends in the resolve step within seconds. A main whose history
was rewritten under the last nightly fails every tick, naming the nightly and
its commit, until someone dispatches a nightly from main to restart the train.

To cut one now, dispatch the workflow from main with channel `nightly` (the
default). A dispatch skips all three checks.

GitHub disables a scheduled workflow after 60 days without activity in the
repository; re-enable it in the Actions tab if that happens.

## How stable is promoted

Dispatch the workflow from main with channel `stable`. It finds the newest
published nightly, reads the commit it shipped from its release body, refuses
if that commit is not on main, and builds that commit, not main's head. The
stable's version is the nightly's with the train dropped
(`0.5.0-nightly.20260923.41` ships as `0.5.0`); set the `version` input to ship
any other `X.Y.Z` above the latest stable. The `vX.Y.Z` tag is created on the
nightly's commit when the release publishes.

A promotion runs the release scripts of the commit it builds (the workflow
file comes from main), so only promote nightlies cut by the current pipeline.

## The hotfix route

Push a tag `vX.Y.Z` on main's first-parent history to build and publish exactly
that commit as stable. The resolve step refuses, before anything is built, a
tag on any other commit: one main never had, or one main only reaches through
a merge (a commit from inside a merged branch). So merge the fix to main first,
then tag the commit the merge put on main. The version must be above the latest
published stable.
`package.json` is not consulted: it stays at the development baseline and every
build stamps its own version.

## Builds and publishing

Every entry point builds the same files. `<slug>` is `productName` from
`tauri.conf.json` in lower case (`fairspoken`); `<Name>` is `MV_DISPLAY_NAME`
in `apps/macos/Config/Base.xcconfig` (`Fairspoken`); `<v>` the version.

| Job (runner) | Files |
| --- | --- |
| `package` windows (`windows-2022`) | `<slug>-<v>-windows-x64-setup.exe`, `<slug>-<v>-transcription-host-windows-x64.zip` |
| `package` linux (`ubuntu-24.04`) | `<slug>-<v>-linux-x86_64.AppImage`, `<slug>-<v>-linux-x86_64.deb`, `<slug>-<v>-transcription-host-linux-x64.tar.gz` |
| `package` linux-arm64 (`ubuntu-24.04-arm`) | `<slug>-<v>-transcription-host-linux-arm64.tar.gz` (no Linux arm64 desktop app) |
| `package` macos-arm64 (`macos-15`) | `<slug>-<v>-transcription-host-macos-arm64.tar.gz` (signed and notarized) |
| `macos-app` (`macos-26`, Xcode 26.6) | `<Name>-<v>-macos-arm64.zip` and `.dmg`, `<Name>-Server-<v>-macos-arm64.zip` and `.dmg`, `sparkle-fairspoken.json`, `sparkle-fairspoken-server.json`; without the Apple secrets the apps are `-unsigned` and carry no Sparkle metadata |
| `publish` | `desktop-manifest.json`, `host-manifest.json` (merged from every leg's share) |
| `feeds` | the six feeds on `update-feeds` |

No release carries `latest.json` or `nightly.json`: nothing installed reads them.

Each signature is checked before it leaves its runner: every desktop updater
payload and host archive against `plugins.updater.pubkey` in `tauri.conf.json`
(`scripts/release/minisign.mjs`), each Sparkle zip against the `SUPublicEDKey`
its app ships. A private key that does not match the public key the apps carry
fails the build, not every installed update.

The legs publish nothing. One publisher waits for packaging and the CI gate
(`ci.yml` against the exact commit), merges the manifests, uploads everything
to a draft, publishes it, and checks it with the job's token: every product
and metadata file present, every manifest usable, every Sparkle length
matching its zip (`verify-release`). The `feeds` job then renders and uploads
the feeds and reads them back with no credentials at the exact URLs installed
apps use (`verify-public`), which is the check a private or misnamed repository
fails.

The release body records `<!-- source-sha: -->`. The nightly gate and stable
promotion both read it back, so do not remove it. Nightlies past the newest
three are deleted after each nightly's feeds are up; their tags stay.

Set dispatch `publish=false` to keep the packages as workflow artifacts for 14
days without publishing anything.

### The transcription host archive layout

The host's self-updater downloads the archive named in `host-<channel>.json`,
checks its `sha256` and its minisign `signature` (the Tauri updater key, the
same public key the desktop app ships), and only then extracts the binary.
Every archive holds exactly one top-level folder named like the archive without
its extension:

| Platform key | Archive (`format`) | Binary inside |
| --- | --- | --- |
| `linux-x86_64` | `<slug>-<v>-transcription-host-linux-x64.tar.gz` (`tar.gz`) | `<slug>-<v>-transcription-host-linux-x64/transcription-host` |
| `linux-aarch64` | `<slug>-<v>-transcription-host-linux-arm64.tar.gz` (`tar.gz`) | `<slug>-<v>-transcription-host-linux-arm64/transcription-host` |
| `windows-x86_64` | `<slug>-<v>-transcription-host-windows-x64.zip` (`zip`) | `<slug>-<v>-transcription-host-windows-x64/transcription-host.exe` |
| `darwin-aarch64` | `<slug>-<v>-transcription-host-macos-arm64.tar.gz` (`tar.gz`) | `<slug>-<v>-transcription-host-macos-arm64/transcription-host` |

Beside the binary: `PROTOCOL.md`, `LICENSE`, `THIRD_PARTY_NOTICES.md`,
`fairspoken-transcription-host.service` (systemd) and
`ie.fairspoken.transcription-host.plist` (launchd). The tar.gz binaries keep
their executable bit. The zip is made by PowerShell's `Compress-Archive`;
extract by matching the entry whose last path component is
`transcription-host.exe` inside the one folder, accepting `/` or `\` as the
separator, rather than by exact entry name. `hostArchiveLayout` in
`scripts/release/release-lib.mjs` is the definition.

The Linux binaries need glibc 2.38 or later (the prebuilt ONNX Runtime), so
Ubuntu 24.04 or newer. The Windows desktop installer is NSIS only: WiX cannot
package a nightly version.

## The native macOS apps

`apps/macos/scripts/release.sh` builds Release, signs it with the Developer ID
Application identity and the hardened runtime, notarizes and staples it, and
writes a zip (the Sparkle enclosure: `ditto -c -k --sequesterRsrc --keepParent`
of the stapled app) and a DMG for people (signed, notarized and stapled too).
`--app client` (the default) packages `Fairspoken.app`; `--app server`
packages `Fairspoken Server.app` as `<Name>-Server-...`.

- **Version.** `CFBundleShortVersionString` is the release version written whole
  (`0.5.0-nightly.20261004.41`). `CFBundleVersion` is the build number,
  `--build-number YYYYMMDDHHMM` (default: now, UTC), passed to xcodebuild as
  `CURRENT_PROJECT_VERSION`; the script refuses an app whose `CFBundleVersion`
  came out different.
- **Sparkle.** With an EdDSA key (`SPARKLE_ED_PRIVATE_KEY` in the environment,
  or `--sparkle-key-file`) a notarized build's zip is signed with Sparkle's
  `sign_update` (`$SPARKLE_BIN/sign_update`, else on `PATH`; CI installs Sparkle
  2.10.0 from the official tarball, pinned by checksum) and `sparkle-<app>.json`
  is written beside it. An app without `SUPublicEDKey` in its Info.plist cannot
  take Sparkle updates and gets no metadata, with a warning.
- **Identity.** Nightly and stable share `ie.fairspoken.mac` (the server
  `ie.fairspoken.server`): macOS keys Microphone and Accessibility grants on the
  bundle id and the signing identity, so moving between channels keeps them.
  Debug builds stay `ie.fairspoken.mac.dev`.
- **Toolchain.** The macOS 26 SDK: CI and the release pin
  `DEVELOPER_DIR=/Applications/Xcode_26.6.app` on `macos-26` and a checksummed
  XcodeGen 2.46.0; bump both together in `ci.yml` and `release.yml`.

To make the same files on a Mac:

```sh
# once: store the notarization credentials in the login keychain
xcrun notarytool store-credentials fairspoken-notary --apple-id <apple id> --team-id <TEAM_ID>

cd apps/macos
scripts/release.sh --version 0.2.0              # → build/release/Fairspoken-0.2.0-macos-arm64.{zip,dmg}
scripts/release.sh --app server --version 0.2.0 # → build/release/Fairspoken-Server-0.2.0-macos-arm64.{zip,dmg}
SPARKLE_BIN=/path/to/Sparkle-2.10.0/bin scripts/release.sh --version 0.2.0 \
  --sparkle-key-file ~/.sparkle/fairspoken_ed25519.private-seed.b64   # also sparkle-fairspoken.json
scripts/release.sh --no-notarize                # sign only, files end -unnotarized
scripts/release.sh --unsigned                   # ad-hoc, files end -unsigned (what CI does without secrets)
```

`shared/host-conformance/run.mjs` checks a running host against the wire
protocol: `node shared/host-conformance/run.mjs --url <host url> --token <token>`.

## Continuous integration

`.github/workflows/ci.yml` runs on every pull request and every push to main,
and as the release's quality gate:

| Check | Runner | Covers |
| --- | --- | --- |
| Build & test | `ubuntu-24.04` | frontend, `npm run test:release`, `npm run test:scripts` (`scripts/test-*.mjs`), the whole crate (host bin included) and `cargo test`, the Parakeet-only build |
| Build & test (macOS) | `macos-latest` | the crate with the macOS-only code, `cargo test` |
| Build & test (Windows) | `windows-2022` | the crate and both bins, `cargo test --lib` |
| Build transcription host (Linux arm64) | `ubuntu-24.04-arm` | `cargo build --bin transcription-host` |
| Build & test (macOS app) | `macos-26` | `apps/macos/scripts/test.sh` (FairspokenCore and FairspokenHost unit tests), `scripts/build.sh` (both apps, Debug, ad-hoc signed) |
| Dependency licenses | `ubuntu-latest` | `cargo deny check licenses` |

There are no path filters: a required check that a filter skips never reports,
and the pull request waits on it forever. Third-party actions are pinned to
commit SHAs (with the version in a comment); GitHub's own `actions/*` follow
their major tags.

## Secrets

Checked in the resolve job, before any runner is spent, from whether each is
set (never its value). `TAURI_SIGNING_PRIVATE_KEY` is required. Every other
group is all-or-none: a partial group fails the run before anything is built;
an empty group ships that platform unsigned (or, for Sparkle, without an
appcast entry) with a warning. The Apple and Azure names are
sprintengine/studio's, so the same values carry over.

| Secret | Group | Value, and how to produce it |
| --- | --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | required | The contents of the Tauri updater private key file (the `~/.tauri/<name>.key` that `npx tauri signer generate -w ~/.tauri/<name>.key` wrote), whose public half is `plugins.updater.pubkey` in `tauri.conf.json`: `pbcopy < ~/.tauri/<name>.key`. Signs the desktop updater payloads and the host archives. |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | with the above | Its password; leave unset if the key has none. |
| `CSC_LINK` | Apple | A "Developer ID Application" certificate with its private key, exported from Keychain Access (My Certificates → Export as `.p12` with a password), in base64: `base64 -i cert.p12 \| pbcopy`. |
| `CSC_KEY_PASSWORD` | Apple | The `.p12` export password. |
| `APPLE_ID` | Apple | The Apple ID that notarizes. |
| `APPLE_APP_SPECIFIC_PASSWORD` | Apple | An app-specific password for it (appleid.apple.com → Sign-In and Security). |
| `APPLE_TEAM_ID` | Apple | The ten-character team id (`<TEAM_ID>`). |
| `SPARKLE_ED_PRIVATE_KEY` | Sparkle | The base64 32-byte Ed25519 seed whose public key is `SUPublicEDKey` in both apps' Info.plist (`SPGwzOHQk1H5lvqePdjg11xTC8LT3/AX3kILlbOIxp0=`): the single line in `~/.sparkle/fairspoken_ed25519.private-seed.b64`, which Sparkle's `sign_update --ed-key-file` reads as is. `pbcopy < ~/.sparkle/fairspoken_ed25519.private-seed.b64`. Used only when the Apple group is complete. |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TRUSTED_SIGNING_ENDPOINT`, `AZURE_TRUSTED_SIGNING_ACCOUNT_NAME`, `AZURE_TRUSTED_SIGNING_CERTIFICATE_PROFILE_NAME` | Azure | Windows Authenticode signing through Azure Trusted Signing: the service principal (tenant, client id, client secret) with the "Trusted Signing Certificate Profile Signer" role, and the account's endpoint, name and certificate profile. Without them the installer is unsigned and SmartScreen warns on first run. |

Set them with `gh secret set <NAME> --repo sprintengine/fairspoken` (it reads
the value from stdin, so `gh secret set SPARKLE_ED_PRIVATE_KEY < ~/.sparkle/...`
never echoes it). Secret values never reach a log: certificates go to a
temporary keychain that is deleted, the Sparkle key goes to `sign_update` on
stdin.

Without the Apple secrets, macOS builds are ad-hoc signed with a warning: the
native apps are named `-unsigned`, get no appcast entry, and the macOS host
binaries are neither Developer ID signed nor notarized. macOS treats each
ad-hoc build as a new app, so do not ship stable that way.

**Neither signing key can be replaced.** Every installed build trusts only the
public keys it shipped with: the Tauri key for the desktop app and the host,
the Sparkle key for the Mac apps. Lose a private key and those installs can
never update again; keep backups outside this machine.

## First release on the new repository

1. **Repository settings.** Public (installed apps read the feeds without
   credentials; `verify-public` fails on a private repository). Set
   `squash_merge_commit_title=PR_TITLE` and `squash_merge_commit_message=PR_BODY`,
   since the squashed PR title decides the next version. Actions: allow
   workflows to create releases (Settings → Actions → Workflow permissions:
   read and write).
2. **Tag.** `v0.1.0` must be in the repository (`git push origin v0.1.0`) so
   the first nightly derives its version from it (0.2.0 with any `feat` since).
   No GitHub release is needed for it.
3. **Secrets.** Set every secret above (at least `TAURI_SIGNING_PRIVATE_KEY`;
   all of Apple plus `SPARKLE_ED_PRIVATE_KEY` for Mac updates).
4. **Keys match the apps.** `tauri.conf.json` `plugins.updater.pubkey` is the
   public half of the Tauri key, its endpoint is
   `https://github.com/sprintengine/fairspoken/releases/download/update-feeds/desktop-stable.json`,
   and both Mac apps' `SUPublicEDKey` is
   `SPGwzOHQk1H5lvqePdjg11xTC8LT3/AX3kILlbOIxp0=`. The release fails, before
   publishing, if a signature does not verify against these.
5. **Dry run.** `node scripts/release/release.mjs feeds-dry-run` renders all six
   feeds from fixtures; `npm run test:release` passes.
6. **Build without publishing.**
   `gh workflow run release.yml --ref main -f channel=nightly -f publish=false`,
   then `gh run watch`. Every leg must pass (artifacts are kept 14 days).
7. **First nightly.** `gh workflow run release.yml --ref main -f channel=nightly`.
   The `feeds` job creates the `update-feeds` release and uploads
   `desktop-nightly.json`, `host-nightly.json` and both appcasts (stable JSON
   feeds appear with the first stable). Check
   `curl -sL https://github.com/sprintengine/fairspoken/releases/download/update-feeds/desktop-nightly.json`.
8. **First stable.** `gh workflow run release.yml --ref main -f channel=stable`
   promotes that nightly; all six feeds now exist.
9. From then on the schedule cuts nightlies by itself.

## Failure and rollback

- A run that failed after publishing its release but before or during the
  feeds: re-run the failed jobs, or dispatch channel `feeds`.
- A bad release: delete it on GitHub and dispatch channel `feeds`; the feeds
  re-render from what is left. Installed builds that already took it stay on it.
- For a bad stable, publish a patch release: installed builds never move
  backwards, so a fix forward is the only rollback that reaches them.
- Update check errors show in each app's Settings → Updates.
