#!/bin/sh
# Package one built lumberroom binary into the .tar.gz a release carries.
#
#   scripts/package-archive.sh <built-binary> <target-triple> <version> <out-dir>
#
# Writes $out-dir/lumberroom-$version-$target.tar.gz holding the binary under its plain PATH name,
# LICENSE and README.md. Prints the archive path.
#
# This exists as its own file because three callers need the identical bytes: scripts/cli-release.sh
# for the local legs, and both release jobs in .github/workflows/cli-release.yml. A Homebrew formula
# pins a sha256 per archive, so two callers producing archives that differ by a timestamp or a uid
# produce two different hashes for the same build, and whichever one the formula was written against
# is the only one that installs. Copies of tar flags in three places is how that drift starts.
#
# Reproducible on bsdtar, verified by building the same input twice and comparing bytes. The GNU tar
# branch uses the equivalent flags and has not been compared against the bsdtar output; `--format=ustar`
# narrows the gap and is not proof that it closes it.
set -eu

BIN="${1:?usage: package-archive.sh <binary> <target> <version> <out-dir>}"
TARGET="${2:?missing target triple}"
VERSION="${3:?missing version}"
OUT="${4:?missing out dir}"

# A fixed date rather than the build's own clock, so the only thing that changes between two runs of
# the same commit is nothing.
ARCHIVE_MTIME=202401010000.00

REPO_DIR="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

case "$(tar --version 2>&1 | head -1)" in
  *bsdtar*) OWNER_FLAGS="--uid 0 --gid 0" ;;
  *"GNU tar"*) OWNER_FLAGS="--owner=0 --group=0" ;;
  *) OWNER_FLAGS="" ;;
esac

mkdir -p "$OUT"
stage="$OUT/.stage-$TARGET"
rm -rf "$stage"
mkdir -p "$stage"

cp "$BIN" "$stage/lumberroom"
cp "$REPO_DIR/LICENSE" "$stage/LICENSE"
cp "$REPO_DIR/README.md" "$stage/README.md"
chmod 755 "$stage/lumberroom"
chmod 644 "$stage/LICENSE" "$stage/README.md"
TZ=UTC touch -t "$ARCHIVE_MTIME" "$stage/lumberroom" "$stage/LICENSE" "$stage/README.md"

archive="$OUT/lumberroom-$VERSION-$TARGET.tar.gz"
if [ -n "$OWNER_FLAGS" ]; then
  # shellcheck disable=SC2086
  tar --format=ustar $OWNER_FLAGS --numeric-owner -cf - \
    -C "$stage" lumberroom LICENSE README.md | gzip -n -9 > "$archive"
else
  echo "warning: tar is neither bsdtar nor GNU tar; archiving without owner-zeroing flags, byte reproducibility not guaranteed" >&2
  tar -cf - -C "$stage" lumberroom LICENSE README.md | gzip -n -9 > "$archive"
fi

rm -rf "$stage"
echo "$archive"
