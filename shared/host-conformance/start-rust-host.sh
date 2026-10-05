#!/usr/bin/env bash
# Starts the Rust transcription host for a conformance run, with a throwaway
# config file so dashboard/config edits never touch the real host config.
# Runs in the foreground; Ctrl-C (or killing this script) stops the host and
# deletes the temp config.
#
#   shared/host-conformance/start-rust-host.sh <port> <token>
#
# Other FAIRSPOKEN_HOST_* variables pass through, e.g.
#   FAIRSPOKEN_HOST_WORKERS=1 FAIRSPOKEN_HOST_QUEUE_CAPACITY=2 start-rust-host.sh 48970 secret
# Set HOST_BIN to use another binary.

set -euo pipefail

if [[ $# -lt 1 || $# -gt 2 ]]; then
  echo "usage: $0 <port> [token]" >&2
  exit 2
fi
port=$1
token=${2:-}

repo=$(cd "$(dirname "$0")/../.." && pwd)
bin=${HOST_BIN:-$repo/src-tauri/target/release/transcription-host}
if [[ ! -x $bin ]]; then
  echo "no host binary at $bin; build it with: (cd src-tauri && cargo build --release --bin transcription-host)" >&2
  exit 1
fi
if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
  echo "port $port is already in use:" >&2
  lsof -nP -iTCP:"$port" -sTCP:LISTEN >&2
  exit 1
fi

config_dir=$(mktemp -d "${TMPDIR:-/tmp}/host-conformance.XXXXXX")
pid=
cleanup() {
  if [[ -n $pid ]] && kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$config_dir"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

env_token=()
[[ -n $token ]] && env_token=(FAIRSPOKEN_HOST_TOKEN="$token")
env FAIRSPOKEN_HOST_ADDR="127.0.0.1:$port" \
  FAIRSPOKEN_HOST_CONFIG_PATH="$config_dir/host-config.json" \
  ${env_token[@]+"${env_token[@]}"} \
  "$bin" &
pid=$!
echo "transcription-host pid $pid on http://127.0.0.1:$port (config $config_dir/host-config.json)"
wait "$pid"
