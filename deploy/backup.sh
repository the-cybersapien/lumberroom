#!/usr/bin/env bash
# Daily local backup: one age-encrypted pg_dump per day, 14 days retained.  [PRD §8, Phase 3]
#
#   ./deploy/backup.sh                    # write backups/lumberroom-YYYY-MM-DD.sql.gz.age
#   ./deploy/backup.sh --restore <file>   # decrypt (if needed) and restore that dump
#
# A plaintext dump exposes private content exactly as a stolen disk would — that is the whole
# point of Phase 3's sensitivity axis, and a backup that undoes it silently is worse than no
# backup. This script refuses to write a plaintext dump: no `age` binary and no configured
# recipient is a hard stop, not a fallback. Set BACKUP_ALLOW_PLAINTEXT=true to opt back in for a
# local/dev box that holds no real data; it prints a warning every time it is used.
#
# The recipient here is deliberately not the KEK. The KEK decrypts `private` rows inside the
# running server; a dump encrypted to it would let anyone holding both the dump and the KEK (the
# same disk, in most of the failure modes worth planning for) decrypt everything at rest twice
# over instead of needing two separate compromises. Generate a dedicated pair:
#   age-keygen -o backup-key.txt
# Put the public line (age1...) in BACKUP_AGE_RECIPIENT on the box. Keep backup-key.txt off the
# box entirely — it is only ever needed to restore, from wherever you keep it.

set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_DIR"

BACKUP_DIR="${BACKUP_DIR:-$REPO_DIR/backups}"
RETAIN_DAYS="${BACKUP_RETAIN_DAYS:-14}"

# Read only the two keys this script needs, rather than sourcing the whole file with `sh`.
# .env holds values like AUTH_TOKENS and OWNER_PASSWORD_HASH that contain characters (`"`, `$`)
# a full `. ./.env` would mangle or try to expand — see the AUTH_TOKENS comment in .env.example.
# install.sh's env_set single-quotes everything it writes precisely to make sourcing safe, but
# this script has no reason to depend on that discipline holding for a hand-edited .env too.
env_get() {
  local v
  v="$(grep -E "^$1=" .env 2>/dev/null | tail -1 | cut -d= -f2-)"
  v="${v%\'}"; v="${v#\'}"
  printf '%s' "$v"
}
DB_USER="${POSTGRES_USER:-$(env_get POSTGRES_USER)}"; DB_USER="${DB_USER:-lumberroom}"
DB_NAME="${POSTGRES_DB:-$(env_get POSTGRES_DB)}"; DB_NAME="${DB_NAME:-lumberroom}"
AGE_RECIPIENT="${BACKUP_AGE_RECIPIENT:-$(env_get BACKUP_AGE_RECIPIENT)}"
AGE_RECIPIENTS_FILE="${BACKUP_AGE_RECIPIENTS_FILE:-$(env_get BACKUP_AGE_RECIPIENTS_FILE)}"
ALLOW_PLAINTEXT="${BACKUP_ALLOW_PLAINTEXT:-$(env_get BACKUP_ALLOW_PLAINTEXT)}"; ALLOW_PLAINTEXT="${ALLOW_PLAINTEXT:-false}"

mkdir -p "$BACKUP_DIR"

recipient_args() {
  # BACKUP_AGE_RECIPIENT is comma- or space-separated age1... public keys; BACKUP_AGE_RECIPIENTS_FILE
  # is a -R file, one per line. Either or both.
  local r
  if [ -n "$AGE_RECIPIENT" ]; then
    for r in $(printf '%s' "$AGE_RECIPIENT" | tr ',' ' '); do printf -- '-r\n%s\n' "$r"; done
  fi
  [ -n "$AGE_RECIPIENTS_FILE" ] && printf -- '-R\n%s\n' "$AGE_RECIPIENTS_FILE"
}

can_encrypt() {
  command -v age >/dev/null 2>&1 && { [ -n "$AGE_RECIPIENT" ] || [ -n "$AGE_RECIPIENTS_FILE" ]; }
}

if [ "${1:-}" = "--restore" ]; then
  FILE="${2:?usage: backup.sh --restore <file.sql.gz.age|file.sql.gz>}"
  [ -f "$FILE" ] || { echo "no such file: $FILE" >&2; exit 1; }
  echo "restoring $FILE into $DB_NAME — this overwrites current data"
  read -r -p "type 'restore' to continue: " confirm
  [ "$confirm" = restore ] || { echo "aborted"; exit 1; }

  case "$FILE" in
    *.age)
      IDENTITY="${BACKUP_AGE_IDENTITY:-}"
      [ -n "$IDENTITY" ] || {
        echo "BACKUP_AGE_IDENTITY is not set. This dump is encrypted and the private key never" >&2
        echo "lives on this box by design — decrypt it wherever you keep backup-key.txt, or copy" >&2
        echo "that key here temporarily: BACKUP_AGE_IDENTITY=/path/to/backup-key.txt $0 --restore $FILE" >&2
        exit 1
      }
      command -v age >/dev/null 2>&1 || { echo "age is not installed; cannot decrypt $FILE" >&2; exit 1; }
      age -d -i "$IDENTITY" "$FILE" | gunzip -c | docker compose exec -T db psql -U "$DB_USER" -d "$DB_NAME"
      ;;
    *.gz)
      echo "warning: this is an unencrypted dump (pre-encryption, or BACKUP_ALLOW_PLAINTEXT was used)" >&2
      gunzip -c "$FILE" | docker compose exec -T db psql -U "$DB_USER" -d "$DB_NAME"
      ;;
    *)
      echo "unrecognised extension on $FILE, expected .sql.gz.age or .sql.gz" >&2; exit 1 ;;
  esac
  echo "restored. Restart the server: docker compose restart server"
  exit 0
fi

STAMP="$(date +%F)"

if can_encrypt; then
  OUT="$BACKUP_DIR/lumberroom-$STAMP.sql.gz.age"
  # Read recipient_args' output one line per array element, so each -r/-R flag and its argument
  # stays a separate word; splitting on whitespace instead would break on a recipients file path
  # containing a space.
  ARGS=()
  while IFS= read -r line; do ARGS+=("$line"); done < <(recipient_args)
  docker compose exec -T db pg_dump -U "$DB_USER" -d "$DB_NAME" --clean --if-exists \
    | gzip -9 \
    | age -e "${ARGS[@]}" > "$OUT.partial"
elif [ "$ALLOW_PLAINTEXT" = true ]; then
  echo "warning: BACKUP_ALLOW_PLAINTEXT=true — writing an unencrypted dump. Private content, if any, is in the clear in this file." >&2
  OUT="$BACKUP_DIR/lumberroom-$STAMP.sql.gz"
  docker compose exec -T db pg_dump -U "$DB_USER" -d "$DB_NAME" --clean --if-exists \
    | gzip -9 > "$OUT.partial"
else
  echo "refusing to write a plaintext backup." >&2
  if ! command -v age >/dev/null 2>&1; then
    echo "age is not installed. Install it (see https://github.com/FiloSottile/age)." >&2
  fi
  if [ -z "$AGE_RECIPIENT" ] && [ -z "$AGE_RECIPIENTS_FILE" ]; then
    echo "no recipient configured. Set BACKUP_AGE_RECIPIENT=age1... in .env (age-keygen -o backup-key.txt)." >&2
  fi
  echo "set BACKUP_ALLOW_PLAINTEXT=true instead only if this box holds no real data." >&2
  exit 1
fi
mv "$OUT.partial" "$OUT"
chmod 600 "$OUT"

SIZE="$(du -h "$OUT" | cut -f1)"
echo "$(date -Iseconds) backup ok $OUT ($SIZE)"

# Prune old dumps (both extensions, so a history that predates encryption still ages out), but
# never leave the directory empty.
find "$BACKUP_DIR" \( -name 'lumberroom-*.sql.gz.age' -o -name 'lumberroom-*.sql.gz' \) -type f -mtime "+$RETAIN_DAYS" -print -delete

COUNT="$(find "$BACKUP_DIR" \( -name 'lumberroom-*.sql.gz.age' -o -name 'lumberroom-*.sql.gz' \) -type f | wc -l | tr -d ' ')"
echo "$(date -Iseconds) retained $COUNT dumps in $BACKUP_DIR"
