#!/usr/bin/env bash
# Run Fairspoken Server headless under launchd, as a per-user LaunchAgent: it starts when you log
# in (no window, menu bar item or Dock icon) and launchd restarts it if it exits abnormally.
#
#   scripts/server-agent.sh install [--app PATH] [--env KEY=VALUE ...]
#   scripts/server-agent.sh status
#   scripts/server-agent.sh uninstall
#
# --app       the app to run (default: /Applications/Fairspoken Server.app, else ~/Applications/…)
# --env       extra environment for the server, e.g. --env FAIRSPOKEN_HOST_ADDR=0.0.0.0:48173
#             (see apps/macos/README.md; normally the app's own host-config.json is enough)
#
# The agent's label is the app's bundle id; its log is ~/Library/Logs/Fairspoken Server.log.
# Don't run the app normally at the same time: both would try to bind the same port.
set -euo pipefail
ACTION="${1:-}"; shift || true
APP=""
ENVS=()
while (($#)); do
  case "$1" in
    --app) APP="${2:?}"; shift 2 ;;
    --env) ENVS+=("${2:?}"); shift 2 ;;
    *) echo "server-agent.sh: unknown argument $1" >&2; exit 1 ;;
  esac
done
if [[ -z "$APP" ]]; then
  for candidate in "/Applications/Fairspoken Server.app" "$HOME/Applications/Fairspoken Server.app"; do
    [[ -d "$candidate" ]] && APP="$candidate" && break
  done
fi
[[ "$ACTION" == "uninstall" || "$ACTION" == "status" || -d "$APP" ]] || { echo "server-agent.sh: Fairspoken Server.app not found; pass --app PATH" >&2; exit 1; }
BUNDLE_ID="ie.fairspoken.server"
if [[ -d "$APP" ]]; then BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist")"; fi
LABEL="$BUNDLE_ID"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
DOMAIN="gui/$(id -u)"
LOG="$HOME/Library/Logs/Fairspoken Server.log"

case "$ACTION" in
  install)
    EXE="$APP/Contents/MacOS/$(/usr/libexec/PlistBuddy -c 'Print :CFBundleExecutable' "$APP/Contents/Info.plist")"
    mkdir -p "$HOME/Library/LaunchAgents" "$HOME/Library/Logs"
    xml() { sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' <<<"$1"; }
    {
      echo '<?xml version="1.0" encoding="UTF-8"?>'
      echo '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">'
      echo '<plist version="1.0"><dict>'
      echo "  <key>Label</key><string>$(xml "$LABEL")</string>"
      echo "  <key>ProgramArguments</key><array><string>$(xml "$EXE")</string><string>--headless</string></array>"
      echo '  <key>RunAtLoad</key><true/>'
      echo '  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>'
      echo '  <key>ThrottleInterval</key><integer>10</integer>'
      # Transcription is interactive work: don't let launchd treat it as background.
      echo '  <key>ProcessType</key><string>Interactive</string>'
      echo "  <key>StandardOutPath</key><string>$(xml "$LOG")</string>"
      echo "  <key>StandardErrorPath</key><string>$(xml "$LOG")</string>"
      if ((${#ENVS[@]})); then
        echo '  <key>EnvironmentVariables</key><dict>'
        for kv in "${ENVS[@]}"; do echo "    <key>$(xml "${kv%%=*}")</key><string>$(xml "${kv#*=}")</string>"; done
        echo '  </dict>'
      fi
      echo '</dict></plist>'
    } >"$PLIST"
    plutil -lint "$PLIST" >/dev/null
    launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
    launchctl bootstrap "$DOMAIN" "$PLIST"
    echo "Installed $PLIST; log: $LOG"
    ;;
  status)
    launchctl print "$DOMAIN/$LABEL" 2>/dev/null | grep -E "state =|pid =|last exit code" || echo "$LABEL is not loaded"
    ;;
  uninstall)
    launchctl bootout "$DOMAIN/$LABEL" 2>/dev/null || true
    rm -f "$PLIST"
    echo "Removed $LABEL"
    ;;
  *) sed -n '2,15p' "$0"; exit 1 ;;
esac
