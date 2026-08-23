#!/bin/sh
# Run the lumberroom client binary against this machine's transcripts.
#
# There is no local Rust toolchain, so the binary is built for Linux inside the builder image and
# runs there too. The transcript directories mount at their real host paths and HOME is set to
# match, because the watermark keys on file_path: mount them anywhere else and every run looks like
# a new corpus.
#
#   ./scripts/lumberroom.sh ingest plan --project lumberroom --since 7d --max-files 40
set -e
cd "$(dirname "$0")/.."
[ -f .env ] && { set -a; . ./.env; set +a; }

[ -x target/release/lumberroom ] || { echo "build it first: ./scripts/cargo.sh build --release -p lumberroom" >&2; exit 1; }

# The ingest credential is the one client with mayIngest, and it is read out of AUTH_TOKENS here so
# it never has to be pasted into a shell history.
TOKEN=$(python3 -c "
import json,os,sys
d=json.loads(os.environ['AUTH_TOKENS'])
m=[c for c in d if c['client']=='${LUMBERROOM_INGEST_CLIENT:-lumberroom}']
sys.stdout.write(m[0]['token'] if m else '')
")
[ -n "$TOKEN" ] || { echo "no ${LUMBERROOM_INGEST_CLIENT:-lumberroom} credential in AUTH_TOKENS" >&2; exit 2; }

mkdir -p "$HOME/.local/state/lumberroom"

# `docker run -e KEY=VALUE` puts VALUE on this process's own argv, which `ps -axww` reads back for
# any local account while the build (a few seconds) or a long ingest run is in flight. Exporting the
# secrets first and passing `-e KEY` with no `=value` makes docker copy them out of this shell's
# environment instead, so nothing sensitive ever appears as a command-line argument. LUMBERROOM_URL
# and the base URL are not secrets and stay inline.
export HOME LUMBERROOM_TOKEN="$TOKEN" ZAI_API_KEY="${ZAI_API_KEY:-}"

exec docker run --rm -i --network "${LUMBERROOM_DOCKER_NETWORK:-lumberroom_default}" \
  -v "$PWD/target/release/lumberroom:/usr/local/bin/lumberroom:ro" \
  -v "$HOME/.claude:$HOME/.claude:ro" \
  -v "$HOME/.codex:$HOME/.codex:ro" \
  -v "$HOME/.local/state/lumberroom:$HOME/.local/state/lumberroom" \
  -e HOME \
  -e "LUMBERROOM_URL=${LUMBERROOM_CLI_URL:-http://server:8787}" \
  -e LUMBERROOM_TOKEN \
  -e ZAI_API_KEY \
  -e "ZAI_BASE_URL=${ZAI_BASE_URL:-}" \
  -w "$PWD" \
  debian:trixie-slim lumberroom "$@"
