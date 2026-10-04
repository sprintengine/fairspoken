#!/usr/bin/env bash
# Build the apps from the command line.
#   scripts/build.sh                      # Debug, both apps
#   scripts/build.sh release              # Release, both apps
#   scripts/build.sh --app server         # Debug, Fairspoken Server only (client | server | all)
#
# Products (Debug shown; Release is the same under Release/):
#   build/Build/Products/Debug/Fairspoken.app          ie.fairspoken.mac.dev     (scheme MultiVoice)
#   build/Build/Products/Debug/Fairspoken Server.app   ie.fairspoken.server.dev  (scheme FairspokenServer)
#
# Anything else is passed to xcodebuild as build settings and overrides the xcconfigs
# (Config/Local.xcconfig included):
#   scripts/build.sh MV_SIGN_IDENTITY=-                  # ad-hoc, ignoring Local.xcconfig
#   scripts/build.sh release MARKETING_VERSION=1.2.3     # what scripts/release.sh does
# CI needs neither: with no Config/Local.xcconfig (it is git-ignored) the build is
# ad-hoc signed, which needs no keychain identity.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
CONFIG="Debug"
APP="all"
SETTINGS=()
while (($#)); do
  case "$1" in
    release) CONFIG="Release"; shift ;;
    debug) CONFIG="Debug"; shift ;;
    --app) APP="${2:?--app needs client, server or all}"; shift 2 ;;
    *) SETTINGS+=("$1"); shift ;;
  esac
done
case "$APP" in
  client) SCHEMES=(MultiVoice) ;;
  server) SCHEMES=(FairspokenServer) ;;
  all) SCHEMES=(MultiVoice FairspokenServer) ;;
  *) echo "build.sh: --app must be client, server or all" >&2; exit 1 ;;
esac
command -v xcodegen >/dev/null || { echo "xcodegen missing: brew install xcodegen" >&2; exit 1; }
xcodegen generate --quiet
for scheme in "${SCHEMES[@]}"; do
  xcodebuild -project MultiVoice.xcodeproj -scheme "$scheme" -configuration "$CONFIG" \
    -derivedDataPath build -destination 'platform=macOS,arch=arm64' -skipPackagePluginValidation -quiet \
    ${SETTINGS[@]+"${SETTINGS[@]}"} build
done
for scheme in "${SCHEMES[@]}"; do
  if [[ "$scheme" == "MultiVoice" ]]; then name="$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Base.xcconfig)"; else name="$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Server.xcconfig)"; fi
  app="build/Build/Products/$CONFIG/$name.app"
  echo "Built: $app"
  codesign -dv "$app" 2>&1 | grep -E "Identifier|Authority|TeamIdentifier" || true
done
