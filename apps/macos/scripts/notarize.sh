#!/usr/bin/env bash
# Developer ID signing and notarization plumbing shared by release.sh and the release workflow
# (.github/workflows/release.yml: the macOS transcription host leg and the macos-app job), so the
# keychain import and Apple's retry rules live in one place:
#
#   notarize.sh import-keychain KEYCHAIN LOCK_SECONDS
#       Creates KEYCHAIN (a new file) with a random password, imports the Developer ID
#       Application certificate from $CSC_LINK (a base64 .p12) with $CSC_KEY_PASSWORD, lets
#       codesign use it without a prompt, and puts it first in the user search list. It locks
#       after LOCK_SECONDS idle. The .p12 is decoded to a temporary file that is always removed.
#       The caller deletes the keychain when it is done (security delete-keychain).
#
#   notarize.sh check
#       Signs in to Apple's notary service once (notarytool history), before a build rather than
#       after it. Apple's answer names the cause (wrong password, unknown team, agreement not
#       accepted) and is printed. A 5xx is Apple's notary service having a bad minute, so that
#       alone is retried: 4 attempts, 30 s, 60 s, 90 s apart.
#
#   notarize.sh submit FILE RESULT_JSON
#       Submits FILE and waits for the verdict (45 minutes at most), writing notarytool's JSON
#       answer to RESULT_JSON. No submission id means the upload itself failed (Apple's 5xx), so
#       that is resubmitted: 3 attempts, 60 s, then 120 s apart. A verdict on a submission is
#       final. Exits 0 only when the status is Accepted; otherwise prints the answer and Apple's
#       log for the submission, and exits 1.
#
# Credentials for check and submit, the first that is complete:
#   APPLE_ID + APPLE_APP_SPECIFIC_PASSWORD + APPLE_TEAM_ID    CI: the repository secrets
#   the notarytool keychain profile $NOTARY_PROFILE (default fairspoken-notary); see release.sh.
set -euo pipefail

die() { echo "notarize.sh: $*" >&2; exit 1; }

notary_auth() {
  if [[ -n "${APPLE_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
    NOTARY_AUTH=(--apple-id "$APPLE_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD" --team-id "$APPLE_TEAM_ID")
    NOTARY_AUTH_NAME="the APPLE_ID credentials"
  else
    NOTARY_AUTH=(--keychain-profile "${NOTARY_PROFILE:-fairspoken-notary}")
    NOTARY_AUTH_NAME="the notarytool keychain profile ${NOTARY_PROFILE:-fairspoken-notary}"
  fi
}

import_keychain() {
  local keychain="${1:?import-keychain needs a keychain path}" lock="${2:?import-keychain needs a lock timeout in seconds}"
  [[ -n "${CSC_LINK:-}" && -n "${CSC_KEY_PASSWORD:-}" ]] || die "CSC_LINK and CSC_KEY_PASSWORD must be set"
  [[ ! -e "$keychain" ]] || die "$keychain already exists"
  local password p12
  password="$(openssl rand -hex 24)"
  p12="$(mktemp "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/signing.XXXXXX")"
  # shellcheck disable=SC2064 # expand now: p12 is local to this function
  trap "rm -f '$p12'" EXIT
  printf '%s' "$CSC_LINK" | base64 --decode >"$p12"
  security create-keychain -p "$password" "$keychain"
  security set-keychain-settings -lut "$lock" "$keychain"
  security unlock-keychain -p "$password" "$keychain"
  security import "$p12" -k "$keychain" -P "$CSC_KEY_PASSWORD" -T /usr/bin/codesign >/dev/null
  rm -f "$p12"
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$password" "$keychain" >/dev/null
  # shellcheck disable=SC2046 # one keychain path per word
  security list-keychains -d user -s "$keychain" $(security list-keychains -d user | tr -d '"')
}

check() {
  notary_auth
  echo "Notarizing with $NOTARY_AUTH_NAME."
  local attempt output
  for attempt in 1 2 3 4; do
    output="$(xcrun notarytool history "${NOTARY_AUTH[@]}" 2>&1 >/dev/null)" && return 0
    echo "$output" >&2
    if [[ "$attempt" == 4 ]] || ! grep -q 'HTTP status code: 5[0-9][0-9]' <<<"$output"; then
      die "notarytool cannot sign in. Create the profile (see release.sh --help), set APPLE_ID/APPLE_APP_SPECIFIC_PASSWORD/APPLE_TEAM_ID, or pass --no-notarize."
    fi
    echo "Apple's notary service failed (attempt $attempt of 4); retrying in $((attempt * 30)) s." >&2
    sleep $((attempt * 30))
  done
}

submit() {
  local file="${1:?submit needs a file}" result="${2:?submit needs a result path}" attempt id status
  notary_auth
  echo "Notarizing $(basename "$file") (this waits for Apple)..."
  for attempt in 1 2 3; do
    xcrun notarytool submit "$file" "${NOTARY_AUTH[@]}" --wait --timeout 45m --output-format json >"$result" || true
    id="$(plutil -extract id raw -o - "$result" 2>/dev/null || true)"
    [[ -n "$id" || "$attempt" == 3 ]] && break
    echo "The notary upload got no submission id (attempt $attempt of 3); retrying in $((attempt * 60)) s." >&2
    sleep $((attempt * 60))
  done
  status="$(plutil -extract status raw -o - "$result" 2>/dev/null || echo "no answer")"
  if [[ "$status" != "Accepted" ]]; then
    cat "$result" >&2 || true
    if [[ -n "$id" ]]; then xcrun notarytool log "$id" "${NOTARY_AUTH[@]}" >&2 || true; fi
    die "notarization of $(basename "$file") ended: $status"
  fi
  echo "Notarized $(basename "$file") ($id)."
}

command="${1:-}"
shift || true
case "$command" in
  import-keychain) import_keychain "$@" ;;
  check) check ;;
  submit) submit "$@" ;;
  *) die "usage: notarize.sh import-keychain KEYCHAIN LOCK_SECONDS | check | submit FILE RESULT_JSON" ;;
esac
