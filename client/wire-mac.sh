#!/usr/bin/env bash
# Wire this Mac (or any machine running Claude Code) to a deployed lumberroom.  [PRD §6]
#
#   LUMBERROOM_TOKEN=<token> ./client/wire-mac.sh --url https://memory.example.com   # --token-mode
#   ./client/wire-mac.sh --url https://memory.example.com --oauth-mode              # Logto/built-in OAuth
#   LUMBERROOM_TOKEN=<token> ./client/wire-mac.sh --url ... --dry-run   # show every change, touch nothing
#
# It does four things, each idempotent:
#   1. writes ~/.config/lumberroom/config.json (mode 600) so lumberroom knows the endpoint
#   2. installs lumberroom (one already on PATH, else the released binary) and the SessionStart
#      hook script under ~/.local/bin and ~/.claude/hooks
#   3. registers the MCP server with Claude Code and adds the SessionStart hook to settings.json
#   4. appends the memory rules to ~/.claude/CLAUDE.md between managed markers
#
# --token-mode (default) needs the AUTH_TOKENS value for this client, read from LUMBERROOM_TOKEN or,
# with a terminal attached and no LUMBERROOM_TOKEN, prompted with echo off. Not a --token flag: every
# argument of a running process is world-readable through `ps`, and a value on the command line also
# lands in shell history. --oauth-mode needs no token here: Claude Code negotiates its own OAuth
# client against the server's discovery metadata when it connects, and the lumberroom CLI on this
# machine gets its own separate credential by running `lumberroom login` after this script finishes.
# The two modes are not a preference toggle, they hand out two different kinds of credential to two
# different consumers of the same endpoint.
#
# Every file it edits is backed up next to the original with a .lumberroom.bak suffix.

set -euo pipefail

URL=""
TOKEN=""
MODE="token"
SCOPE="user"
DRY_RUN=0
CLIENT_NAME="lumberroom"
CLAUDE_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
BIN_DIR="${LUMBERROOM_BIN_DIR:-$HOME/.local/bin}"
CONFIG_DIR="${LUMBERROOM_CONFIG_DIR:-$HOME/.config/lumberroom}"
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE_REPO="${LUMBERROOM_RELEASE_REPO:-https://github.com/the-cybersapien/lumberroom}"
RELEASE_VERSION="${LUMBERROOM_RELEASE_VERSION:-0.3.0}"

usage() {
  sed -n '2,23p' "$0" | sed 's/^# \{0,1\}//'
  exit "${1:-0}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --url) URL="${2:?--url needs a value}"; shift 2 ;;
    --token) echo "--token is not accepted: it would sit in argv (readable through ps) and in shell history. Set LUMBERROOM_TOKEN instead, or pipe the token on stdin." >&2; exit 1 ;;
    --name) CLIENT_NAME="${2:?--name needs a value}"; shift 2 ;;
    --scope) SCOPE="${2:?--scope needs a value}"; shift 2 ;;
    --token-mode) MODE="token"; shift ;;
    --oauth-mode) MODE="oauth"; shift ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage 0 ;;
    *) echo "unknown flag: $1" >&2; usage 1 ;;
  esac
done

[ -n "$URL" ] || { echo "--url is required (e.g. https://memory.example.com)" >&2; exit 1; }
case "$MODE" in
  token)
    TOKEN="${LUMBERROOM_TOKEN:-}"
    if [ -z "$TOKEN" ] && [ -t 0 ]; then
      read -r -s -p "AUTH_TOKENS value for this client: " TOKEN
      echo >&2
    fi
    if [ -z "$TOKEN" ] && [ ! -t 0 ]; then
      TOKEN="$(head -n 1)"
    fi
    [ -n "$TOKEN" ] || { echo "no token: set LUMBERROOM_TOKEN, run in a terminal to be prompted, or pipe the token on stdin" >&2; exit 1; }
    ;;
  oauth) : ;;
esac

URL="${URL%/}"
MCP_URL="$URL"
case "$MCP_URL" in */mcp) ;; *) MCP_URL="$URL/mcp" ;; esac

command -v jq >/dev/null 2>&1 || { echo "jq is required. brew install jq" >&2; exit 1; }
command -v node >/dev/null 2>&1 || { echo "node is required (>=20)." >&2; exit 1; }
if [ "$MODE" = "oauth" ]; then
  command -v curl >/dev/null 2>&1 || { echo "curl is required for --oauth-mode's reachability check." >&2; exit 1; }
fi

say() { printf '%s\n' "$*"; }
# A dry run's whole purpose is to be safe to paste into a chat log or read back in a terminal
# recording. Any of the four steps below can carry $TOKEN in their argv or file body; redact it
# before it ever reaches a `say` or `run` line, not after, since the un-redacted string otherwise
# has already been written to stdout by the time a later step could scrub it.
redact() {
  if [ -n "$TOKEN" ]; then printf '%s\n' "$1" | sed "s#$(printf '%s' "$TOKEN" | sed 's/[.[\*^$/]/\\&/g')#<token>#g"
  else printf '%s\n' "$1"; fi
}
run() {
  if [ "$DRY_RUN" = 1 ]; then say "  would run: $(redact "$*")"; else "$@"; fi
}
backup() {
  [ -f "$1" ] || return 0
  if [ "$DRY_RUN" = 1 ]; then say "  would back up $1 -> $1.lumberroom.bak"; else cp "$1" "$1.lumberroom.bak"; fi
}
write_file() {
  # write_file <path> <mode> <<<content
  local path="$1" mode="$2" content
  content="$(cat)"
  if [ "$DRY_RUN" = 1 ]; then
    say "  would write $path (mode $mode):"
    redact "$content" | sed 's/^/    /'
    return 0
  fi
  # The write runs under umask 077 so a file that does not exist yet is born owner-only. Without
  # it the redirect created config.json at the caller's umask, usually 0644, with the token
  # already inside, and a script interrupted before the chmod left it that way. The chmod still
  # runs: the umask cannot loosen a file that already exists, and the mode asked for may be wider
  # than 0600 for the files that are not credentials.
  mkdir -p "$(dirname "$path")"
  ( umask 077; printf '%s\n' "$content" > "$path" )
  chmod "$mode" "$path"
}

say "lumberroom wiring"
say "  endpoint: $MCP_URL"
say "  auth:     $MODE"
say "  scope:    $SCOPE"
[ "$DRY_RUN" = 1 ] && say "  MODE:     dry run, nothing will change"

# ── 1. lumberroom config ──────────────────────────────────────────────────────────
say ""
say "1/4 lumberroom config -> $CONFIG_DIR/config.json"
if [ "$MODE" = "token" ]; then
  write_file "$CONFIG_DIR/config.json" 600 <<JSON
{
  "url": "$MCP_URL",
  "token": "$TOKEN"
}
JSON
else
  # No token field: an oauth block goes here once `lumberroom login` runs, and a stray "token" would
  # win over it silently (bin/lumberroom.mjs prefers a static token over oauth on purpose).
  write_file "$CONFIG_DIR/config.json" 600 <<JSON
{
  "url": "$MCP_URL"
}
JSON
fi

# ── 2. binaries ───────────────────────────────────────────────────────────────
# Two ways to get a `lumberroom` binary onto this Mac: one already on PATH from Homebrew or a
# manual install, or the released binary for this Mac's architecture.
#
# `bin/lumberroom.mjs` is not one of them. It was the prototype, it answers to the same command
# name, and installing it as a fallback is how a machine ends up running something that looks like
# lumberroom and is missing half of it. It stays in the repository because it is a client the
# server cannot accidentally accommodate, and it has caught protocol bugs the Rust tests could not.
# Nothing installs it.
install_lumberroom_binary() {
  # An earlier run of this script installed bin/lumberroom.mjs to exactly this name, so on any
  # machine already wired, `command -v lumberroom` finds the prototype. Accepting it would leave
  # every existing user on the client this change exists to retire, and the upgrade would report
  # success. A node script announces itself in its first line; a Rust binary does not.
  if command -v lumberroom >/dev/null 2>&1; then
    local existing
    existing="$(command -v lumberroom)"
    if head -c 200 "$existing" 2>/dev/null | head -1 | grep -q node; then
      say "  found the node prototype at $existing; replacing it"
    else
      say "  found lumberroom on PATH: $existing"
      if [ "$existing" != "$BIN_DIR/lumberroom" ]; then
        run ln -sf "$existing" "$BIN_DIR/lumberroom"
      fi
      say "  chose: PATH ($existing)"
      return 0
    fi
  fi

  if ! command -v curl >/dev/null 2>&1; then
    say "  curl not found; skipping the released binary"
    return 1
  fi
  local arch target archive url tmp_dir
  arch="$(uname -m)"
  case "$arch" in
    arm64|aarch64) target="aarch64-apple-darwin" ;;
    x86_64) target="x86_64-apple-darwin" ;;
    *) say "  no released binary for architecture $arch"; return 1 ;;
  esac
  archive="lumberroom-${RELEASE_VERSION}-${target}.tar.gz"
  url="${RELEASE_REPO}/releases/download/v${RELEASE_VERSION}/${archive}"
  if [ "$DRY_RUN" = 1 ]; then
    say "  would download $url and install it to $BIN_DIR/lumberroom"
    say "  chose: released binary (dry run, not fetched)"
    return 0
  fi
  tmp_dir="$(mktemp -d)"
  if curl -fsSL "$url" -o "$tmp_dir/$archive" 2>/dev/null \
    && tar -xzf "$tmp_dir/$archive" -C "$tmp_dir" lumberroom 2>/dev/null; then
    install -m 755 "$tmp_dir/lumberroom" "$BIN_DIR/lumberroom"
    rm -rf "$tmp_dir"
    say "  downloaded $url"
    say "  chose: released binary"
    return 0
  fi
  rm -rf "$tmp_dir"
  say "  release $url not reachable"
  return 1
}

say ""
say "2/4 lumberroom -> $BIN_DIR/lumberroom, hook -> $CLAUDE_DIR/hooks/lumberroom-bootstrap.sh"
run mkdir -p "$BIN_DIR" "$CLAUDE_DIR/hooks"
if ! install_lumberroom_binary; then
  say ""
  say "  no lumberroom binary is available on this machine. Get one, then run this again:"
  say "    brew install the-cybersapien/lumberroom/lumberroom"
  say "    cargo install lumberroom"
  say "    or download it from ${RELEASE_REPO}/releases"
  exit 1
fi
run install -m 755 "$REPO_DIR/client/lumberroom-bootstrap-hook.sh" "$CLAUDE_DIR/hooks/lumberroom-bootstrap.sh"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) say "  note: $BIN_DIR is not on PATH. Add it to your shell profile to use lumberroom directly." ;;
esac

# ── 3. Claude Code: MCP server + SessionStart hook ────────────────────────────
say ""
say "3/4 Claude Code registration"
if claude mcp get "$CLIENT_NAME" >/dev/null 2>&1; then
  say "  MCP server '$CLIENT_NAME' already registered; replacing it"
  run claude mcp remove "$CLIENT_NAME" --scope "$SCOPE" || true
fi
if [ "$MODE" = "token" ]; then
  run claude mcp add --transport http "$CLIENT_NAME" "$MCP_URL" \
    --scope "$SCOPE" --header "Authorization: Bearer $TOKEN"
else
  # No header: Claude Code detects the 401 plus WWW-Authenticate on first connect and runs its
  # own OAuth registration and consent flow against the server's discovery metadata. That client
  # is entirely separate from the one `lumberroom login` mints for the CLI below.
  run claude mcp add --transport http "$CLIENT_NAME" "$MCP_URL" --scope "$SCOPE"
fi

SETTINGS="$CLAUDE_DIR/settings.json"
HOOK_CMD="$CLAUDE_DIR/hooks/lumberroom-bootstrap.sh"
backup "$SETTINGS"
if [ ! -f "$SETTINGS" ]; then
  write_file "$SETTINGS" 644 <<'JSON'
{}
JSON
fi
if [ "$DRY_RUN" = 1 ]; then
  say "  would add SessionStart hook: $HOOK_CMD"
else
  tmp="$(mktemp)"
  # Append our hook to the existing SessionStart array without disturbing the others.
  jq --arg cmd "$HOOK_CMD" '
    .hooks //= {} |
    .hooks.SessionStart //= [] |
    if ([.hooks.SessionStart[]?.hooks[]?.command] | index($cmd)) then .
    else .hooks.SessionStart += [{"hooks":[{"type":"command","command":$cmd,"timeout":10}]}]
    end
  ' "$SETTINGS" > "$tmp" && mv "$tmp" "$SETTINGS"
  say "  SessionStart hook installed in $SETTINGS"
fi

# ── 4. CLAUDE.md write rule ───────────────────────────────────────────────────
say ""
say "4/4 memory rules -> $CLAUDE_DIR/CLAUDE.md"
CLAUDE_MD="$CLAUDE_DIR/CLAUDE.md"
SNIPPET="$REPO_DIR/client/CLAUDE.md.snippet"
if [ -f "$CLAUDE_MD" ] && grep -q 'lumberroom:begin' "$CLAUDE_MD"; then
  say "  markers already present; refreshing the block"
  if [ "$DRY_RUN" = 0 ]; then
    backup "$CLAUDE_MD"
    tmp="$(mktemp)"
    awk -v snippet="$SNIPPET" '
      /lumberroom:begin/ { while ((getline line < snippet) > 0) print line; skip=1; next }
      /lumberroom:end/   { skip=0; next }
      skip != 1 { print }
    ' "$CLAUDE_MD" > "$tmp" && mv "$tmp" "$CLAUDE_MD"
  fi
else
  if [ "$DRY_RUN" = 1 ]; then
    say "  would append $(wc -l < "$SNIPPET" | tr -d ' ') lines to $CLAUDE_MD"
  else
    backup "$CLAUDE_MD"
    mkdir -p "$CLAUDE_DIR"
    printf '\n' >> "$CLAUDE_MD"
    cat "$SNIPPET" >> "$CLAUDE_MD"
    say "  appended"
  fi
fi

# ── verify ────────────────────────────────────────────────────────────────────
say ""
if [ "$DRY_RUN" = 1 ]; then
  say "dry run complete. Re-run without --dry-run to apply."
  exit 0
fi
if [ "$MODE" = "token" ]; then
  say "verifying..."
  if LUMBERROOM_URL="$MCP_URL" LUMBERROOM_TOKEN="$TOKEN" "$BIN_DIR/lumberroom" doctor; then
    say ""
    say "wired. Start a new Claude Code session and run: /mcp   (the 'lumberroom' server should be connected)"
    say "The SessionStart hook now injects the digest automatically."
  else
    say ""
    say "wiring is in place but the server did not answer. Check the URL, the token, and TLS."
    exit 1
  fi
else
  say "checking the endpoint is reachable (no credential to verify with yet)..."
  # healthz hangs off the HTTP root, not /mcp, and --url is accepted either way (line 47-49).
  HEALTH_BASE="${URL%/mcp}"
  if curl -fsS -o /dev/null "${HEALTH_BASE}/healthz"; then
    say "  healthz: reachable"
  else
    say ""
    say "wiring is in place but ${HEALTH_BASE}/healthz did not answer. Check the URL and TLS before logging in."
    exit 1
  fi
  say ""
  say "wired. Two logins remain, each against its own OAuth client:"
  say "  1. Start a new Claude Code session and run: /mcp   (it will prompt you to sign in)"
  say "  2. Run: LUMBERROOM_URL=$MCP_URL $BIN_DIR/lumberroom login    (so the lumberroom CLI and the hook can call the server directly)"
  say "The SessionStart hook needs step 2 done at least once; until then it fails open and adds no digest."
fi
