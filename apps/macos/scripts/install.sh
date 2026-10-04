#!/usr/bin/env bash
# Build Release and install to ~/Applications (no admin needed), then launch it.
#   scripts/install.sh                 # Fairspoken
#   scripts/install.sh --app server    # Fairspoken Server
#   scripts/install.sh --app all       # both
set -euo pipefail
cd "$(dirname "$0")/.."
APP="client"
if [[ "${1:-}" == "--app" ]]; then APP="${2:?--app needs client, server or all}"; fi
scripts/build.sh release --app "$APP"
names=()
case "$APP" in
  client) names=("$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Base.xcconfig)") ;;
  server) names=("$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Server.xcconfig)") ;;
  all) names=("$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Base.xcconfig)" "$(sed -nE 's/^MV_DISPLAY_NAME = (.*)$/\1/p' Config/Server.xcconfig)") ;;
esac
mkdir -p "$HOME/Applications"
for name in "${names[@]}"; do
  SRC="build/Build/Products/Release/$name.app"
  DEST="$HOME/Applications/$name.app"
  osascript -e "quit app \"$name\"" 2>/dev/null || true
  pkill -x "$name" 2>/dev/null || true
  rm -rf "$DEST"
  ditto "$SRC" "$DEST"
  echo "Installed: $DEST"
  open "$DEST"
done
