#!/usr/bin/env bash
# Build a Release of one app, sign it with Developer ID, notarize and staple it, and package it:
#   <out>/Fairspoken-<version>-macos-arm64.zip          the stapled app, zipped with ditto
#   <out>/Fairspoken-<version>-macos-arm64.dmg          the stapled app plus an Applications link; the
#                                                        DMG is itself signed, notarized and stapled
#   <out>/Fairspoken-Server-<version>-macos-arm64.zip   the same for Fairspoken Server (--app server)
#   <out>/Fairspoken-Server-<version>-macos-arm64.dmg
#   <out>/sparkle-fairspoken.json (sparkle-fairspoken-server.json)   with a Sparkle key: what the
#                                                        appcast item for the zip is rendered from
# .github/workflows/release.yml runs this same script, so a local run makes the same files.
#
#   scripts/release.sh                                  sign + notarize Fairspoken (the default)
#   scripts/release.sh --app server                     the same for Fairspoken Server
#   scripts/release.sh --version 0.2.0 --build-number 202610051230
#   scripts/release.sh --sparkle-key-file ~/.sparkle/<key>.private-seed.b64   also sign for Sparkle
#   scripts/release.sh --no-notarize                    sign only; files end -unnotarized (checks the signing)
#   scripts/release.sh --unsigned                       ad-hoc, no notarization; files end -unsigned
#   scripts/release.sh --out DIR                        default: build/release (relative to apps/macos)
#
# Signing identity: $FS_SIGN_IDENTITY, else the first "Developer ID Application" identity in the
# keychain search list. $FS_KEYCHAIN limits the search (and codesign) to one keychain; CI sets it
# to the temporary keychain it imports the CSC_LINK certificate into.
#
# Notarization credentials (read by scripts/notarize.sh), the first that is complete:
#   APPLE_ID + APPLE_APP_SPECIFIC_PASSWORD + APPLE_TEAM_ID    CI: the repository secrets
#   the notarytool keychain profile $NOTARY_PROFILE (default fairspoken-notary). Create it once:
#     xcrun notarytool store-credentials fairspoken-notary --apple-id <apple id> --team-id <TEAM_ID>
#   It asks for an app-specific password (appleid.apple.com) and keeps it in the login keychain.
#
# Version: --version is CFBundleShortVersionString, written whole, so a nightly reads
# X.Y.Z-nightly.DATE.RUN in the app as it does in the release. --build-number is CFBundleVersion,
# passed to xcodebuild as CURRENT_PROJECT_VERSION: the UTC build timestamp YYYYMMDDHHMM (the
# default is now), which only ever grows and is what Sparkle compares. The release workflow passes
# the one build number its resolve step chose for every product of the release.
#
# Bundle id: ie.fairspoken.mac (Fairspoken Server: ie.fairspoken.server) on every channel. A nightly is told apart by its version, not by a
# second bundle id: macOS keys Microphone and Accessibility grants on the bundle id and the signing
# identity, so one id lets a user move between nightly and stable without re-granting (Studio's
# nightlies keep their app id the same way).
#
# Sparkle: the .zip (ditto --sequesterRsrc --keepParent of the notarized, stapled app) is the
# update payload. With an EdDSA key -- $SPARKLE_ED_PRIVATE_KEY (CI: the secret, the base64 32-byte
# Ed25519 seed) or --sparkle-key-file -- a notarized build's zip is signed with Sparkle's
# sign_update ($SPARKLE_BIN/sign_update, else on PATH) and sparkle-<app>.json is written beside
# it: bundle id, CFBundleVersion, CFBundleShortVersionString, LSMinimumSystemVersion, the zip's
# name, length and edSignature, and the app's SUPublicEDKey. The release workflow checks the
# signature against that public key and renders the appcasts from these files (docs/releasing.md).
# An app without SUPublicEDKey cannot take Sparkle updates and gets no metadata.
set -euo pipefail
CALLER_DIR="$PWD"
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"

die() { echo "release.sh: $*" >&2; exit 1; }
warn() { if [[ -n "${GITHUB_ACTIONS:-}" ]]; then echo "::warning::$*"; else echo "warning: $*" >&2; fi; }

MODE="notarize"
APP_KIND="client"
VERSION=""
BUILD_NUMBER=""
SPARKLE_KEY_FILE=""
OUT="build/release"
while (($#)); do
  case "$1" in
    --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
    --build-number) BUILD_NUMBER="${2:?--build-number needs a value}"; shift 2 ;;
    --sparkle-key-file) SPARKLE_KEY_FILE="${2:?--sparkle-key-file needs a path}"; shift 2 ;;
    --out) OUT="${2:?--out needs a value}"; [[ "$OUT" = /* ]] || OUT="$CALLER_DIR/$OUT"; shift 2 ;;
    --app) APP_KIND="${2:?--app needs client or server}"; shift 2 ;;
    --unsigned) MODE="unsigned"; shift ;;
    --no-notarize) MODE="signed"; shift ;;
    -h | --help) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 0 ;;
    *) die "unknown argument $1 (see --help)" ;;
  esac
done

setting() { sed -nE "s/^$1 = (.*)$/\1/p" "${2:-Config/Base.xcconfig}" | head -1; }
CLIENT_NAME="$(setting MV_DISPLAY_NAME)"
case "$APP_KIND" in
  client) NAME="$CLIENT_NAME"; PREFIX="$CLIENT_NAME"; SPARKLE_META="sparkle-fairspoken.json" ;;
  # Files are <client name>-Server-…, which the release verify step expects. The Sparkle metadata
  # names are fixed (SPARKLE_APPS in scripts/release/release-lib.mjs): the appcast URLs are too.
  server)
    NAME="$(setting MV_DISPLAY_NAME Config/Server.xcconfig)"; PREFIX="${CLIENT_NAME}-Server"
    SPARKLE_META="sparkle-fairspoken-server.json" ;;
  *) die "--app must be client or server" ;;
esac
VERSION="${VERSION:-$(setting MARKETING_VERSION)}"
BUILD_NUMBER="${BUILD_NUMBER:-$(date -u +%Y%m%d%H%M)}"
[[ -n "$NAME" && -n "$CLIENT_NAME" ]] || die "MV_DISPLAY_NAME is not set in Config/Base.xcconfig or Config/Server.xcconfig"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || die "not a release version: $VERSION"
[[ "$BUILD_NUMBER" =~ ^[0-9]{12}$ ]] || die "CFBundleVersion must be the UTC build timestamp YYYYMMDDHHMM: $BUILD_NUMBER"
[[ -z "$SPARKLE_KEY_FILE" || -r "$SPARKLE_KEY_FILE" ]] || die "cannot read the Sparkle key file $SPARKLE_KEY_FILE"

case "$MODE" in
  unsigned) SUFFIX="-unsigned" ;;
  signed) SUFFIX="-unnotarized" ;;
  *) SUFFIX="" ;;
esac
BASE="${PREFIX}-${VERSION}-macos-arm64${SUFFIX}"

# --- Signing settings -------------------------------------------------------------------------
IDENTITY="-"
TEAM=""
if [[ "$MODE" == "unsigned" ]]; then
  # Overrides Config/Local.xcconfig too, so a local --unsigned run matches CI's.
  SIGN_SETTINGS=("MV_SIGN_IDENTITY=-" "DEVELOPMENT_TEAM=" "OTHER_CODE_SIGN_FLAGS=--timestamp=none")
else
  IDENTITY="${FS_SIGN_IDENTITY:-}"
  if [[ -z "$IDENTITY" ]]; then
    if [[ -n "${FS_KEYCHAIN:-}" ]]; then
      IDENTITIES="$(security find-identity -v -p codesigning "$FS_KEYCHAIN")"
    else
      IDENTITIES="$(security find-identity -v -p codesigning)"
    fi
    IDENTITY="$(sed -nE 's/^ *[0-9]+\) [0-9A-F]+ "(Developer ID Application: .*)"$/\1/p' <<<"$IDENTITIES" | head -1)"
  fi
  [[ -n "$IDENTITY" ]] || die "no \"Developer ID Application\" identity in the keychain (or pass --unsigned)"
  # A Developer ID Application certificate names its team in the parentheses.
  TEAM="${APPLE_TEAM_ID:-$(sed -nE 's/.*\(([A-Z0-9]{10})\)$/\1/p' <<<"$IDENTITY")}"
  [[ -n "$TEAM" ]] || die "cannot tell the team of $IDENTITY; set APPLE_TEAM_ID"
  # Notarization needs a secure timestamp (Base.xcconfig turns it off for fast local builds) and
  # refuses the get-task-allow entitlement.
  SIGN_FLAGS="--timestamp"
  [[ -n "${FS_KEYCHAIN:-}" ]] && SIGN_FLAGS="$SIGN_FLAGS --keychain $FS_KEYCHAIN"
  SIGN_SETTINGS=("MV_SIGN_IDENTITY=$IDENTITY" "DEVELOPMENT_TEAM=$TEAM" "OTHER_CODE_SIGN_FLAGS=$SIGN_FLAGS"
    "CODE_SIGN_INJECT_BASE_ENTITLEMENTS=NO")
  echo "Signing as $IDENTITY (team $TEAM)."
fi

# --- Notarization credentials, checked before the build rather than after it ------------------
# scripts/notarize.sh holds the sign-in check and the submit-and-wait, with Apple's retry rules;
# the release workflow's host leg uses the same script.
if [[ "$MODE" == "notarize" ]]; then
  scripts/notarize.sh check
fi

notarize() { scripts/notarize.sh submit "$1" "$WORK/notary-$(basename "$1").json"; }

# --- Build ------------------------------------------------------------------------------------
scripts/build.sh release --app "$APP_KIND" "MARKETING_VERSION=$VERSION" "CURRENT_PROJECT_VERSION=$BUILD_NUMBER" "${SIGN_SETTINGS[@]}"
APP="build/Build/Products/Release/$NAME.app"
[[ -d "$APP" ]] || die "the build did not produce $APP"

plist() { /usr/libexec/PlistBuddy -c "Print :$1" "$APP/Contents/Info.plist"; }
[[ "$(plist CFBundleShortVersionString)" == "$VERSION" ]] || die "the app says version $(plist CFBundleShortVersionString), not $VERSION"
# Sparkle orders updates by CFBundleVersion, so the build number must be exactly the one asked for.
[[ "$(plist CFBundleVersion)" == "$BUILD_NUMBER" ]] ||
  die "the app says build $(plist CFBundleVersion), not $BUILD_NUMBER (CFBundleVersion must come from CURRENT_PROJECT_VERSION)"
echo "$(plist CFBundleIdentifier) $(plist CFBundleShortVersionString) ($(plist CFBundleVersion))"

codesign --verify --deep --strict "$APP" || die "$APP fails codesign --verify"
if [[ "$MODE" != "unsigned" ]]; then
  details="$(codesign -dvv "$APP" 2>&1)"
  grep -q "^Authority=Developer ID Application" <<<"$details" || die "$APP is not signed with Developer ID"
  grep -q "^TeamIdentifier=$TEAM$" <<<"$details" || die "$APP is not signed by team $TEAM"
  grep -Eq "flags=.*runtime" <<<"$details" || die "$APP is not signed with the hardened runtime"
  if codesign -d --entitlements - --xml "$APP" 2>/dev/null | grep -q "get-task-allow"; then
    die "$APP carries get-task-allow, which notarization refuses"
  fi
fi

# --- Package ----------------------------------------------------------------------------------
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
STAGED="$WORK/app/$NAME.app"
mkdir -p "$WORK/app" "$OUT"
ditto "$APP" "$STAGED"

if [[ "$MODE" == "notarize" ]]; then
  ditto -c -k --keepParent "$STAGED" "$WORK/submit.zip"
  notarize "$WORK/submit.zip"
  xcrun stapler staple "$STAGED"
  spctl --assess --type execute -vv "$STAGED" 2>&1 | tee "$WORK/spctl.txt"
  grep -q "source=Notarized Developer ID" "$WORK/spctl.txt" || die "Gatekeeper does not accept the notarized app"
fi

rm -f "$OUT/$BASE.zip" "$OUT/$BASE.dmg" "$OUT/$SPARKLE_META"
ditto -c -k --sequesterRsrc --keepParent "$STAGED" "$OUT/$BASE.zip"

# --- Sparkle ----------------------------------------------------------------------------------
# Only a notarized, stapled build is ever offered as an update. The key never reaches a file or
# the log: it goes to sign_update on stdin (or as the file the caller named).
if [[ -n "$SPARKLE_KEY_FILE" || -n "${SPARKLE_ED_PRIVATE_KEY:-}" ]]; then
  PUBLIC_ED_KEY="$(plist SUPublicEDKey 2>/dev/null || true)"
  if [[ "$MODE" != "notarize" ]]; then
    echo "Not signing $BASE.zip for Sparkle: only a notarized build is an update."
  elif [[ -z "$PUBLIC_ED_KEY" ]]; then
    warn "$NAME has no SUPublicEDKey in its Info.plist, so it cannot take Sparkle updates; no $SPARKLE_META."
  else
    SIGN_UPDATE="${SPARKLE_BIN:+$SPARKLE_BIN/}sign_update"
    command -v "$SIGN_UPDATE" >/dev/null || die "no sign_update; set SPARKLE_BIN to the bin folder of a Sparkle 2 release"
    if [[ -n "$SPARKLE_KEY_FILE" ]]; then
      ED_SIGNATURE="$("$SIGN_UPDATE" -p --ed-key-file "$SPARKLE_KEY_FILE" "$OUT/$BASE.zip")"
    else
      ED_SIGNATURE="$(printf '%s' "$SPARKLE_ED_PRIVATE_KEY" | "$SIGN_UPDATE" -p --ed-key-file - "$OUT/$BASE.zip")"
    fi
    [[ "$ED_SIGNATURE" =~ ^[A-Za-z0-9+/]{86}==$ ]] || die "sign_update did not print an EdDSA signature"
    MINIMUM_SYSTEM="$(plist LSMinimumSystemVersion)"
    [[ -n "$MINIMUM_SYSTEM" ]] || die "$NAME has no LSMinimumSystemVersion"
    cat >"$OUT/$SPARKLE_META" <<JSON
{
  "bundleId": "$(plist CFBundleIdentifier)",
  "version": "$(plist CFBundleVersion)",
  "shortVersionString": "$(plist CFBundleShortVersionString)",
  "minimumSystemVersion": "$MINIMUM_SYSTEM",
  "file": "$BASE.zip",
  "length": $(stat -f%z "$OUT/$BASE.zip"),
  "edSignature": "$ED_SIGNATURE",
  "publicEDKey": "$PUBLIC_ED_KEY"
}
JSON
    echo "Signed $BASE.zip for Sparkle (build $(plist CFBundleVersion)); wrote $SPARKLE_META."
  fi
fi

mkdir -p "$WORK/dmg"
ditto "$STAGED" "$WORK/dmg/$NAME.app"
ln -s /Applications "$WORK/dmg/Applications"
# hdiutil sometimes finds the volume busy (Spotlight); a retry is the documented cure.
for attempt in 1 2 3; do
  hdiutil create -volname "$NAME" -srcfolder "$WORK/dmg" -fs HFS+ -format UDZO -ov "$OUT/$BASE.dmg" && break
  [[ "$attempt" == 3 ]] && die "hdiutil could not create the DMG"
  sleep 5
done

if [[ "$MODE" != "unsigned" ]]; then
  if [[ -n "${FS_KEYCHAIN:-}" ]]; then
    codesign --force --timestamp --sign "$IDENTITY" --keychain "$FS_KEYCHAIN" "$OUT/$BASE.dmg"
  else
    codesign --force --timestamp --sign "$IDENTITY" "$OUT/$BASE.dmg"
  fi
fi
if [[ "$MODE" == "notarize" ]]; then
  notarize "$OUT/$BASE.dmg"
  xcrun stapler staple "$OUT/$BASE.dmg"
fi

case "$MODE" in
  unsigned) warn "$NAME $VERSION is ad-hoc signed and not notarized ($BASE.*). Gatekeeper blocks it on other Macs, and every build resets the privacy grants macOS keys on its signature." ;;
  signed) echo "$NAME $VERSION is signed but not notarized ($BASE.*): for checking the signing only." ;;
esac
(cd "$OUT" && shasum -a 256 "$BASE.zip" "$BASE.dmg")
