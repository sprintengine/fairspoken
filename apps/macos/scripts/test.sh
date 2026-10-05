#!/usr/bin/env bash
# Unit tests for every Swift package (pure Swift, no Neural Engine or models needed):
#   FairspokenCore   protocol client (frames, URL policy, headers), SSE parser, host state and
#                    simulator, settings/history compatibility, audio gating
#   FairspokenHost   the server: HTTP codec, routing and auth, queue and workers, SSE fan-out,
#                    config validation, stream errors, a real TCP listener
# Extra arguments go to every `swift test` (e.g. --filter).
set -euo pipefail
cd "$(dirname "$0")/../Packages"
export PATH="/opt/homebrew/bin:/usr/local/bin:$PATH"
for package in FairspokenCore FairspokenHost; do
  echo "== $package"
  (cd "$package" && swift test "$@")
done
