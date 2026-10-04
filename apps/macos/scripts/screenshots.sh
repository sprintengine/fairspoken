#!/usr/bin/env bash
# Render light + dark screenshots of every main screen (demo data) and quit:
#   scripts/screenshots.sh            # both apps
#   scripts/screenshots.sh client     # docs/screenshots/
#   scripts/screenshots.sh server     # docs/screenshots/server-app/
set -euo pipefail
cd "$(dirname "$0")/.."
WHICH="${1:-all}"
shoot() { # <display name> <scheme app arg> <output dir>
  local app="build/Build/Products/Debug/$1.app"
  [[ -d "$app" ]] || scripts/build.sh --app "$2"
  mkdir -p "$3"
  "$app/Contents/MacOS/$1" --screenshots "$PWD/$3"
  ls "$3"
}
if [[ "$WHICH" == "client" || "$WHICH" == "all" ]]; then
  shoot "$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Base.xcconfig)" client docs/screenshots
fi
if [[ "$WHICH" == "server" || "$WHICH" == "all" ]]; then
  shoot "$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Server.xcconfig)" server docs/screenshots/server-app
fi
