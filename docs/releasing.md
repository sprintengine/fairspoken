# Releasing

Fairspoken ships on two trains from `.github/workflows/release.yml`, and
installed Windows and Linux builds of the desktop app update themselves from the
GitHub releases of this repository. The release rules are in `scripts/release/` and covered by
`npm run test:release`.

One train carries every product. Every nightly and every stable is one GitHub
release, built from one commit at one version, holding:

- the **Tauri desktop app** (`src-tauri/`, `src/`) for Windows and Linux, with
  its updater manifest. It is not released for macOS: Macs use the native apps
  below;
- the standalone **transcription host** (`transcription-host`, a bin of the same
  crate) for headless machines, on macOS (arm64 and x64), Windows and Linux;
- the two **native macOS apps** (`apps/macos/`): **Fairspoken**, the dictation
  client, and **Fairspoken Server**, a transcription server with its own
  window.

They share the train because they share the code and the wire protocol
(`src-tauri/src/host/PROTOCOL.md`): a host and the clients of the same release
are always a matched set, and one nightly gate, one promotion and one tag cover
them all. This is also how sprintengine/studio ships every platform from one
workflow and one publish job.

## Channels

**Nightly** is main as it stands, a few times a day: versions
`X.Y.Z-nightly.YYYYMMDD.RUN`, published as GitHub prereleases that are never
marked latest, each carrying `nightly.json` as its updater manifest. **Stable**
is `X.Y.Z`, published as the latest release with `latest.json`, and is always a
build of a commit some nightly already shipped (the hotfix tag below is the one
exception).

`X.Y.Z` in a nightly is the version the next stable would take: the strongest
Conventional Commit since the last stable tag, applied to that tag (`feat` is a
minor, everything else a patch; before 1.0 a breaking change is a minor too, and
1.0.0 is only ever typed in). A version a tag already holds is never reused.

## What a merge to main does

Nothing, until the next nightly. CI gates the pull request; the release
workflow does not listen for pushes to main.

## How a nightly is cut

The workflow wakes every 30 minutes (minutes 7 and 37). A scheduled run
publishes only when both hold:

- at least six hours have passed since the last published nightly, and
- main has commits the last nightly did not ship.

Otherwise the run ends in the resolve step within seconds. A main whose history
was rewritten under the last nightly fails every tick, naming the nightly and
its commit, until someone dispatches a nightly from main to restart the train.

To cut one now, dispatch the workflow from main with channel `nightly` (the
default). A dispatch skips the six-hour and new-commits checks.

## How stable is promoted

Dispatch the workflow from main with channel `stable`. It finds the newest
published nightly, reads the commit it shipped from its release body, refuses
if that commit is not on main, and builds that commit, not main's head. The
stable's version is the nightly's with the train dropped
(`0.5.0-nightly.20260923.41` ships as `0.5.0`); set the `version` input to ship
any other `X.Y.Z` above the latest stable. The `vX.Y.Z` tag is created on the
nightly's commit when the release publishes.

## The hotfix route

Push a tag `vX.Y.Z` on main's first-parent history to build and publish exactly
that commit as stable. The version must be above the latest published stable.
`package.json` is not consulted: it stays at the development baseline and every
build stamps its own version.

## How an installed app picks its channel

This section is about the Tauri app on Windows and Linux; the native macOS apps
have no updater yet (see below).

The app reads its own version (`src-tauri/src/updates.rs`): a `-nightly.` build
follows nightlies, anything else follows stable.

- **Stable** asks the endpoint in `tauri.conf.json`,
  `…/releases/latest/download/latest.json`, which GitHub redirects to the newest
  release not marked prerelease.
- **Nightly** reads `…/releases.atom`, takes the first nightly tag in it, and
  asks that release for `nightly.json`.

It checks 30 seconds after launch and every six hours, downloads in the
background, and installs only when the user clicks **Restart to update** in
Settings → General → Updates. Development builds never update.

The updater endpoint in `tauri.conf.json` is the one place the repository is
named: the release scripts read it too, and refuse to publish anywhere else.

## Builds and publishing

Every entry point builds the same files. `<slug>` is `productName` from
`tauri.conf.json` in lower case (`multivoice-tauri` today); `<Name>` is
`MV_DISPLAY_NAME` in `apps/macos/Config/Base.xcconfig` (`Fairspoken`).

| Job (runner) | Files |
| --- | --- |
| `package` macos-arm64 (`macos-15`) | `<slug>-<v>-transcription-host-macos-arm64.tar.gz` (host only, signed and notarized) |
| `package` macos-x64 (`macos-15-intel`) | `<slug>-<v>-transcription-host-macos-x64.tar.gz` (host only, signed and notarized) |
| `package` windows (`windows-2022`) | `<slug>-<v>-windows-x64-setup.exe` (also the updater payload), `<slug>-<v>-transcription-host-windows-x64.zip` |
| `package` linux (`ubuntu-24.04`) | `<slug>-<v>-linux-x86_64.AppImage` (updater), `.deb`, `<slug>-<v>-transcription-host-linux-x64.tar.gz` |
| `package` linux-arm64 (`ubuntu-24.04-arm`) | `<slug>-<v>-transcription-host-linux-arm64.tar.gz` (host only; there is no Linux arm64 desktop app) |
| `macos-app` (`macos-26`, Xcode 26.6) | `<Name>-<v>-macos-arm64.zip` and `.dmg` (the client), `<Name>-Server-<v>-macos-arm64.zip` and `.dmg` (the server); each `-unsigned` (`<Name>-<v>-macos-arm64-unsigned.*`) when built without the Apple secrets |
| `publish` | `nightly.json` or `latest.json`, the Tauri updater manifest (Windows and Linux entries only) |

There is no Tauri macOS DMG or `.app.tar.gz`, and the updater manifest has no
`darwin-aarch64` or `darwin-x86_64` entry. A Tauri macOS build installed from an
earlier release is therefore never offered another update; move it to the
native app by hand.

The Windows installer is NSIS only: WiX cannot package a nightly version. Each
host archive is a folder holding the binary, `PROTOCOL.md`, the systemd and
launchd files from `packaging/host/`, `LICENSE` and `THIRD_PARTY_NOTICES.md`;
tar.gz keeps the binary executable, Windows gets a zip. The Linux binaries need
glibc 2.38 or later (the prebuilt ONNX Runtime), so Ubuntu 24.04 or newer.

The legs publish nothing. One publisher waits for packaging and the CI
gate (`ci.yml` against the exact commit), merges every leg's share of the
updater manifest, uploads everything to a draft, then publishes it and verifies
it twice: once with the job's token, and once with no credentials against the
exact URLs installed builds read.

The release body records `<!-- source-sha: -->`. The nightly gate and stable
promotion both read it back, so do not remove it. Nightlies past the newest
three are deleted after each nightly publishes; their tags stay.

Set dispatch `publish=false` to keep the packages as workflow artifacts for 14
days without publishing anything.

The verify step also checks that every product is on the release by exact file
name (`missingInstallers` in `scripts/release/release-lib.mjs`): the Windows and
Linux installers, all five host archives (macOS ones included), and the zip and
DMG of both native macOS apps. It warns when a native macOS app on it is the
`-unsigned` build.

A stable promotion rebuilds the nightly's commit with that commit's release
scripts. A nightly cut before the Tauri macOS legs were dropped still expects
the Tauri macOS files and the darwin manifest entries, so cut a new nightly
before promoting.

Label a pull request `preview:mac` to get an ad-hoc signed Apple Silicon DMG of
the Tauri app built from it, linked from a comment on the PR. It is for testing
only and never reaches a release.

## The native macOS apps

`apps/macos/scripts/release.sh` builds Release, signs it with the Developer ID
Application identity and the hardened runtime, notarizes it with `notarytool`,
staples it, and writes a zip and a DMG (the DMG is signed, notarized and stapled
too). `--app client` (the default) packages `Fairspoken.app` as
`<Name>-<version>-macos-arm64.zip` and `.dmg`; `--app server` packages
`Fairspoken Server.app` as `<Name>-Server-<version>-macos-arm64.zip` and `.dmg`.
Both use the same identity, notarization credentials and file suffixes. The
`macos-app` job runs the script twice, once per app, with
`--version <release version> --build-number <run number>` and the same output
folder.

To make the same files on a Mac (this Mac has the identity
`Developer ID Application: Your Name (TEAMID1234)`):

```sh
# once: store the notarization credentials in the login keychain
# (asks for an app-specific password from appleid.apple.com)
xcrun notarytool store-credentials fairspoken-notary --apple-id <apple id> --team-id <TEAM_ID>

cd apps/macos
scripts/release.sh --version 0.2.0              # → build/release/Fairspoken-0.2.0-macos-arm64.{zip,dmg}
scripts/release.sh --app server --version 0.2.0 # → build/release/Fairspoken-Server-0.2.0-macos-arm64.{zip,dmg}
scripts/release.sh --no-notarize                # sign only, files end -unnotarized
scripts/release.sh --unsigned                   # ad-hoc, files end -unsigned (what CI does without secrets)
```

`--no-notarize` and `--unsigned` work the same with `--app server`.

- **Version.** `CFBundleShortVersionString` is the release version written whole
  (a nightly reads `0.5.0-nightly.20261004.41` in the app), and
  `CFBundleVersion` is the release workflow's run number, which only grows.
- **Identity.** Nightly and stable are both `ie.fairspoken.mac` (the server is
  `ie.fairspoken.server` on both), as Studio keeps one app id across channels. macOS keys Microphone and Accessibility grants on
  the bundle id and the signing identity, so moving between a nightly and a
  stable keeps them; a second bundle id would make each channel ask again and
  fight over the same hotkey. Debug builds stay `ie.fairspoken.mac.dev`.
- **Updates.** Not set up. There is no Sparkle feed and the workflow publishes
  none; each build of either app is installed by hand. When Sparkle lands, each
  `.zip` is the payload, `CFBundleVersion` is what it compares, and the appcast (EdDSA-signed,
  a nightly channel and the default stable one) is generated by `release.sh`
  and uploaded beside the other files.
- **Toolchain.** The app needs the macOS 26 SDK. CI and the release pin
  `DEVELOPER_DIR=/Applications/Xcode_26.6.app` on the `macos-26` image and a
  checksummed XcodeGen 2.46.0; bump both together in `ci.yml` and
  `release.yml`.
- **Checking a server.** `shared/host-conformance/run.mjs` is a Node script
  with no dependencies that checks a running host against the wire protocol:
  `node shared/host-conformance/run.mjs --url <host url> --token <token>`. It
  needs a running host with a model installed, so CI does not run it.

## Continuous integration

`.github/workflows/ci.yml` runs on every pull request and every push to main,
and as the release's quality gate:

| Check | Runner | Covers |
| --- | --- | --- |
| Build & test | `ubuntu-24.04` | frontend, `npm run test:release`, the whole crate (host bin included) and `cargo test`, the Parakeet-only build |
| Build & test (macOS) | `macos-latest` | the crate with the macOS-only code, `cargo test` |
| Build & test (Windows) | `windows-2022` | the crate and both bins, `cargo test --lib` |
| Build transcription host (Linux arm64) | `ubuntu-24.04-arm` | `cargo build --bin transcription-host` |
| Build & test (macOS app) | `macos-26` | `apps/macos/scripts/test.sh` (the Swift packages' unit tests: MultiVoiceCore and FairspokenHost), `scripts/build.sh` (Fairspoken and Fairspoken Server, Debug, ad-hoc signed) |
| Dependency licenses | `ubuntu-latest` | `cargo deny check licenses` |

There are no path filters: a required check that a filter skips never reports,
and the pull request waits on it forever.

## One-time setup

**Repository.** It must be public: installed builds read releases without
credentials, and the verify step fails on a private repository. Set
`squash_merge_commit_title=PR_TITLE` and `squash_merge_commit_message=PR_BODY`,
since the squashed PR title decides the next version; the
**Conventional PR title** check enforces its shape.

**Secrets.** The first two are required; the others are all-or-nothing per
platform, and a partial set fails the build. The Apple and Azure names are
sprintengine/studio's, so the same values carry over.

| Secret | Used for |
| --- | --- |
| `TAURI_SIGNING_PRIVATE_KEY` | Signing updater payloads. The contents of the private key whose public half is `plugins.updater.pubkey` in `tauri.conf.json`. |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | Its password (empty if it has none). |
| `CSC_LINK`, `CSC_KEY_PASSWORD` | A "Developer ID Application" certificate with its private key as a base64 `.p12` (`base64 -i cert.p12 \| pbcopy`), and its export password. Signs the macOS transcription hosts and both native macOS apps. |
| `APPLE_ID`, `APPLE_APP_SPECIFIC_PASSWORD`, `APPLE_TEAM_ID` | Notarization of the same: the Apple ID, an app-specific password for it, and the team (`<TEAM_ID>`). |
| `AZURE_TENANT_ID`, `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TRUSTED_SIGNING_ENDPOINT`, `AZURE_TRUSTED_SIGNING_ACCOUNT_NAME`, `AZURE_TRUSTED_SIGNING_CERTIFICATE_PROFILE_NAME` | Windows signing through Azure Trusted Signing. Without them the installer is unsigned and SmartScreen warns on first run. |

Without the Apple secrets, macOS builds are ad-hoc signed with a warning: the
native apps' files are then named `-unsigned`, and the macOS host binaries are
neither Developer ID signed nor notarized. macOS treats each ad-hoc build as a
new app, so every update would ask for Microphone and Accessibility permission
again; do not ship stable that way.

To export the certificate: Keychain Access → My Certificates → "Developer ID
Application: …" → Export as `.p12` with a password; `CSC_LINK` is that file in
base64 and `CSC_KEY_PASSWORD` the password. Secret values never reach a log:
each job writes the certificate to a file, imports it into a temporary
keychain, and deletes both.

**First run.** Push the branch and open a pull request: CI runs every check
above. After it merges, dispatch the first nightly by hand:

```sh
gh workflow run release.yml --ref main -f channel=nightly -f publish=false   # build only, artifacts kept 14 days
gh workflow run release.yml --ref main -f channel=nightly                     # build and publish
gh workflow run release.yml --ref main -f channel=stable                      # promote the latest nightly
gh run watch
```

While the repository is private, use `publish=false` to exercise the whole
matrix: a publishing run makes the release visible and then fails its verify
step, which reads the release without credentials as installed builds do.

**The updater key cannot be replaced.** Every installed build trusts only the
public key it shipped with. Lose the private key and no installed build can
ever update again; keep a backup outside this machine.

## Failure and rollback

- Update check errors show in Settings → General → Updates (the Tauri app).
- Previous installers stay on GitHub Releases.
- For a bad stable, publish a patch release: installed builds never move
  backwards, so a fix forward is the only rollback that reaches them.
