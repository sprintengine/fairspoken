#!/usr/bin/env bash
# Xcode build phase of both apps (project.yml › postBuildScripts), not for running by hand.
#
# Sparkle ships its helpers (Autoupdate, Updater.app, the XPC services) ad-hoc signed. Xcode re-signs
# the embedded Sparkle.framework but not the code inside it, so notarization would refuse them. This
# runs after the frameworks are embedded and before the app itself is signed: it drops the XPC
# services (only sandboxed apps use them; neither app is sandboxed), then signs the helpers and the
# framework with the target's identity, the hardened runtime and OTHER_CODE_SIGN_FLAGS (which carry
# --timestamp, and --keychain on CI, in release builds). Ad-hoc builds sign ad-hoc ("-").
set -euo pipefail
fw="${TARGET_BUILD_DIR:?}/${FRAMEWORKS_FOLDER_PATH:?}/Sparkle.framework"
if [[ ! -d "$fw" ]]; then
  echo "error: Sparkle.framework is not embedded in ${FULL_PRODUCT_NAME:-the app}" >&2
  exit 1
fi
identity="${EXPANDED_CODE_SIGN_IDENTITY:-}"
[[ -n "$identity" ]] || identity="${CODE_SIGN_IDENTITY:--}"
# OTHER_CODE_SIGN_FLAGS is a list of flags ("--timestamp --keychain /path"): split on spaces.
read -r -a flags <<<"${OTHER_CODE_SIGN_FLAGS:-}"

sign() { /usr/bin/codesign --force --sign "$identity" ${flags[@]+"${flags[@]}"} --options runtime "$@"; }

rm -rf "$fw/Versions/B/XPCServices" "$fw/XPCServices"
sign "$fw/Versions/B/Autoupdate"
sign "$fw/Versions/B/Updater.app"
sign "$fw"
echo "Signed Sparkle helpers in ${FULL_PRODUCT_NAME:-the app} as ${identity}"
