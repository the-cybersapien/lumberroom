#!/usr/bin/env bash
# Deletes the oauth-flow-test-* rows scripts/oauth-flow-test.sh left behind while it still had no
# database of its own: three of them, registered between 2026-08-19 10:35 and 10:37 UTC, sat in the
# live oauth_client table with nothing to clean them up. oauth-flow-test.sh now runs against its own
# scratch database and writes nothing here going forward; this script is for the rows the earlier
# runs already left.
#
# The predicate, read out of the gate's own registration call (scripts/oauth-flow-test.sh, step
# 4/13), matches both halves it sets there and nothing else:
#   client_name ~ '^oauth-flow-test-[0-9a-f]{12}$'   ("oauth-flow-test-" + the gate's 12-hex NONCE)
#   software_id = 'lumberroom-oauth-flow-test'
# A row has to satisfy both to be touched. Either alone is not enough to be sure it is this gate's
# and not an operator's own client or something else with a similar name.
#
#   ./scripts/purge-oauth-flow-test-clients.sh              dry run against the live database, lists what matches
#   ./scripts/purge-oauth-flow-test-clients.sh --yes         deletes the rows just listed
#   ./scripts/purge-oauth-flow-test-clients.sh --db NAME     targets a different database (for rehearsal)
#
# Dry run is the default. Nothing is deleted unless --yes is given, and the rows a run would touch
# are always printed first, whether or not --yes is present. This reads the owner's live oauth_client
# table by default; a wrong DELETE there is unrecoverable, which is why there is no default that
# deletes.
#
# oauth_code, oauth_token and oauth_refresh all reference oauth_client(client_id) ON DELETE CASCADE
# (migrations/20260819000007_oauth.sql), so deleting the client row is enough on its own; nothing
# else needs cleaning up by hand.
#
# Runs through `docker compose exec db psql`, the same route the scratch-server library and every
# other rehearsal script in this repo use to reach postgres: no POSTGRES_PASSWORD needed, since it
# relies on the db container's own local trust auth rather than a network connection.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck disable=SC1091
[ -f "$REPO_DIR/.env" ] && { set -a; . "$REPO_DIR/.env"; set +a; }
PG_USER="${POSTGRES_USER:-lumberroom}"
DB="${POSTGRES_DB:-lumberroom}"
YES=0

while [ $# -gt 0 ]; do
  case "$1" in
    --yes) YES=1; shift ;;
    --db) DB="$2"; shift 2 ;;
    -h|--help) sed -n '2,30p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
done

command -v docker >/dev/null 2>&1 || { echo "docker is required" >&2; exit 1; }

compose() { docker compose -f "$REPO_DIR/docker-compose.yml" "$@"; }

psql_() {
  # psql_ SQL: one statement against $DB, tuples-only, unaligned, pipe-separated columns. -q on
  # top of -t: -t alone drops a SELECT's row-count footer but not DELETE's own "DELETE n" status
  # line, which would otherwise land as a bogus extra row in DELETE ... RETURNING's output and
  # throw off the count check below it.
  compose exec -T db psql -U "$PG_USER" -d "$DB" -At -q -F '|' -c "$1"
}

# The pattern the gate registers with. Not escaped for shell expansion beyond what double quotes
# already do: the trailing $ here is a literal dollar (postgres end-of-string anchor), never
# followed by a character bash would read as the start of an expansion.
MATCH_PREDICATE="client_name ~ '^oauth-flow-test-[0-9a-f]{12}$' AND software_id = 'lumberroom-oauth-flow-test'"

if [ "$DB" = "${POSTGRES_DB:-lumberroom}" ]; then
  printf '\033[33m  targeting %s, the live database (no --db was given).\033[0m\n' "$DB"
else
  printf '  targeting %s (--db given)\n' "$DB"
fi
echo "  predicate: client_name ~ '^oauth-flow-test-[0-9a-f]{12}\$' and software_id = 'lumberroom-oauth-flow-test'"
echo ""

ROWS="$(psql_ "SELECT client_id, client_name, created_at, revoked_at FROM oauth_client WHERE $MATCH_PREDICATE ORDER BY created_at")"

if [ -z "$ROWS" ]; then
  echo "  no matching rows in $DB.oauth_client. Nothing to do."
  exit 0
fi

COUNT="$(printf '%s\n' "$ROWS" | grep -c .)"
echo "  $COUNT matching row(s):"
printf '%s\n' "$ROWS" | awk -F'|' '{
  revoked = ($4 == "") ? "" : "  revoked " $4
  printf "    %s  %-32s created %s%s\n", $1, $2, $3, revoked
}'

if [ "$YES" -ne 1 ]; then
  echo ""
  echo "  dry run. Nothing deleted. Re-run with --yes to delete the row(s) listed above."
  exit 0
fi

echo ""
echo "  deleting..."
DELETED="$(psql_ "DELETE FROM oauth_client WHERE $MATCH_PREDICATE RETURNING client_id")"
DELETED_COUNT=0
[ -n "$DELETED" ] && DELETED_COUNT="$(printf '%s\n' "$DELETED" | grep -c .)"

if [ "$DELETED_COUNT" -eq "$COUNT" ]; then
  echo "  deleted $DELETED_COUNT row(s) from $DB.oauth_client:"
  printf '%s\n' "$DELETED" | sed 's/^/    /'
else
  echo "  deleted $DELETED_COUNT row(s), expected $COUNT. Check $DB.oauth_client by hand." >&2
  exit 1
fi
