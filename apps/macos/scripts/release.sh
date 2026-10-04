#!/usr/bin/env bash
# Build a Release of one app, sign it with Developer ID, notarize and staple it, and package it:
#   <out>/Fairspoken-<version>-macos-arm64.zip          the stapled app, zipped with ditto
#   <out>/Fairspoken-<version>-macos-arm64.dmg          the stapled app plus an Applications link; the
#                                                        DMG is itself signed, notarized and stapled
#   <out>/Fairspoken-Server-<version>-macos-arm64.zip   the same for Fairspoken Server (--app server)
#   <out>/Fairspoken-Server-<version>-macos-arm64.dmg
# .github/workflows/release.yml runs this same script, so a local run makes the same files.
#
#   scripts/release.sh                                  sign + notarize Fairspoken (the default)
#   scripts/release.sh --app server                     the same for Fairspoken Server
#   scripts/release.sh --version 0.2.0 --build-number 41
#   scripts/release.sh --no-notarize                    sign only; files end -unnotarized (checks the signing)
#   scripts/release.sh --unsigned                       ad-hoc, no notarization; files end -unsigned
#   scripts/release.sh --out DIR                        default: build/release (relative to apps/macos)
#
# Signing identity: $FS_SIGN_IDENTITY, else the first "Developer ID Application" identity in the
# keychain search list. $FS_KEYCHAIN limits the search (and codesign) to one keychain; CI sets it
# to the temporary keychain it imports the CSC_LINK certificate into.
#
# Notarization credentials, the first that is complete:
#   APPLE_ID + APPLE_APP_SPECIFIC_PASSWORD + APPLE_TEAM_ID    CI: the repository secrets
#   the notarytool keychain profile $NOTARY_PROFILE (default fairspoken-notary). Create it once:
#     xcrun notarytool store-credentials fairspoken-notary --apple-id <apple id> --team-id <TEAM_ID>
#   It asks for an app-specific password (appleid.apple.com) and keeps it in the login keychain.
#
# Version: --version is CFBundleShortVersionString, written whole, so a nightly reads
# X.Y.Z-nightly.DATE.RUN in the app as it does in the release. --build-number is CFBundleVersion;
# the release workflow passes its run number, which only ever grows.
#
# Bundle id: ie.fairspoken.mac (Fairspoken Server: ie.fairspoken.server) on every channel. A nightly is told apart by its version, not by a
# second bundle id: macOS keys Microphone and Accessibility grants on the bundle id and the signing
# identity, so one id lets a user move between nightly and stable without re-granting (Studio's
# nightlies keep their app id the same way).
#
# TODO(updates): Sparkle is not set up. When it is, the .zip is the update payload, CFBundleVersion
# is what Sparkle compares, and the appcast is generated and EdDSA-signed here. Until then there is
# no update feed: users install each build by hand.
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
OUT="build/release"
while (($#)); do
  case "$1" in
    --version) VERSION="${2:?--version needs a value}"; shift 2 ;;
    --build-number) BUILD_NUMBER="${2:?--build-number needs a value}"; shift 2 ;;
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
  client) NAME="$CLIENT_NAME"; PREFIX="$CLIENT_NAME" ;;
  # Files are <client name>-Server-…, which the release verify step expects.
  server) NAME="$(setting MV_DISPLAY_NAME Config/Server.xcconfig)"; PREFIX="${CLIENT_NAME}-Server" ;;
  *) die "--app must be client or server" ;;
esac
VERSION="${VERSION:-$(setting MARKETING_VERSION)}"
BUILD_NUMBER="${BUILD_NUMBER:-$(setting CURRENT_PROJECT_VERSION)}"
[[ -n "$NAME" && -n "$CLIENT_NAME" ]] || die "MV_DISPLAY_NAME is not set in Config/Base.xcconfig or Config/Server.xcconfig"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || die "not a release version: $VERSION"
[[ "$BUILD_NUMBER" =~ ^[0-9]+(\.[0-9]+){0,2}$ ]] || die "CFBundleVersion must be up to three integers: $BUILD_NUMBER"

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
if [[ "$MODE" == "notarize" ]]; then
  if [[ -n "${APPLE_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    NOTARY_AUTH=(--apple-id "$APPLE_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD" --team-id "$APPLE_TEAM_ID")
    echo "Notarizing with the APPLE_ID credentials."
  else
    NOTARY_AUTH=(--keychain-profile "${NOTARY_PROFILE:-fairspoken-notary}")
    echo "Notarizing with the notarytool keychain profile ${NOTARY_PROFILE:-fairspoken-notary}."
  fi
  xcrun notarytool history "${NOTARY_AUTH[@]}" >/dev/null 2>&1 ||
    die "notarytool cannot sign in. Create the profile (see --help), set APPLE_ID/APPLE_APP_SPECIFIC_PASSWORD/APPLE_TEAM_ID, or pass --no-notarize."
fi

notarize() {
  local file="$1" result status id
  result="$WORK/notary-$(basename "$file").json"
  echo "Notarizing $(basename "$file") (this waits for Apple)..."
  xcrun notarytool submit "$file" "${NOTARY_AUTH[@]}" --wait --timeout 45m --output-format json >"$result" || true
  status="$(plutil -extract status raw -o - "$result" 2>/dev/null || echo "no answer")"
  id="$(plutil -extract id raw -o - "$result" 2>/dev/null || true)"
  if [[ "$status" != "Accepted" ]]; then
    cat "$result" >&2 || true
    [[ -n "$id" ]] && xcrun notarytool log "$id" "${NOTARY_AUTH[@]}" >&2 || true
    die "notarization of $(basename "$file") ended: $status"
  fi
  echo "Notarized $(basename "$file") ($id)."
}

# --- Build ------------------------------------------------------------------------------------
scripts/build.sh release --app "$APP_KIND" "MARKETING_VERSION=$VERSION" "CURRENT_PROJECT_VERSION=$BUILD_NUMBER" "${SIGN_SETTINGS[@]}"
APP="build/Build/Products/Release/$NAME.app"
[[ -d "$APP" ]] || die "the build did not produce $APP"

plist() { /usr/libexec/PlistBuddy -c "Print :$1" "$APP/Contents/Info.plist"; }
[[ "$(plist CFBundleShortVersionString)" == "$VERSION" ]] || die "the app says version $(plist CFBundleShortVersionString), not $VERSION"
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

rm -f "$OUT/$BASE.zip" "$OUT/$BASE.dmg"
ditto -c -k --sequesterRsrc --keepParent "$STAGED" "$OUT/$BASE.zip"

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
