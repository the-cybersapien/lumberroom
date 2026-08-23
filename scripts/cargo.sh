#!/bin/sh
# Run cargo against this repo inside the builder image.
#
#   ./scripts/cargo.sh check --all-targets
#   ./scripts/cargo.sh test
#
# The builder image carries g++ (ONNX Runtime links libstdc++) and the OpenSSL headers; a bare
# rust:slim does not. Build it once with:  docker build -t lumberroom-builder -f Dockerfile.builder .
# The integration suite needs the compose database, which is why the container joins its network.
set -e
cd "$(dirname "$0")/.."
[ -f .env ] && { set -a; . ./.env; set +a; }

# `test` links the lib-test and integration binaries in one step. Doing that concurrently gets the
# linker OOM-killed in the container (`collect2: fatal error: ld terminated with signal 9`), which
# reads like a compile error and is a memory ceiling: Docker Desktop here is handing the
# container 6.2GB of the host's 16GB. The real fix is raising that allocation; until someone does,
# force `-j 1` by default so nobody has to rediscover this. Only for `test`, and only when the
# caller has not already picked a `-j`: `check` gets real parallelism because it never links two
# binaries at once.
if [ "$1" = "test" ]; then
  has_j=0
  for arg in "$@"; do
    case "$arg" in
      -j*|--jobs*) has_j=1 ;;
    esac
  done
  if [ "$has_j" = 0 ]; then
    sub="$1"
    shift
    set -- "$sub" -j 1 "$@"
  fi
fi

# ── the container outlives the command that started it, unless something stops it ────────────────
#
# `docker run --rm` removes the container when it exits on its own. It does nothing when this script
# is killed: the signal reaches the docker client, the client dies, and the container keeps running
# with cargo still holding the lock on /app/target. Every later run then sits on "Blocking waiting
# for file lock on build directory" until it is killed too, and leaves another one behind. Two dead
# runs are enough to make the suite look hung for no reason anybody can see.
#
# Two things stop that. A trap removes this run's own container on any signal it can catch, which
# needs the `exec` gone: exec replaces this shell and takes its traps with it. And because SIGKILL
# catches nothing, a sweep first removes any container from an earlier run whose owner is no longer
# alive. The owner's pid rides along as a label, so "no longer alive" is a question with an answer
# rather than a guess about age.

NAME="lumberroom-cargo-$$"

sweep() {
  for c in $(docker ps -q --filter "label=lumberroom.cargo.owner" 2>/dev/null); do
    owner=$(docker inspect -f '{{ index .Config.Labels "lumberroom.cargo.owner" }}' "$c" 2>/dev/null)
    [ -n "$owner" ] || continue
    # A live owner means a real concurrent run, and cargo's own lock is what serialises those.
    if kill -0 "$owner" 2>/dev/null; then
      continue
    fi
    echo "cargo.sh: removing $(docker inspect -f '{{.Name}}' "$c" 2>/dev/null | sed 's|^/||'), left by pid $owner which is gone" >&2
    docker rm -f "$c" >/dev/null 2>&1 || true
  done
}

cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1 || true
}

sweep
trap 'cleanup' EXIT
trap 'cleanup; exit 130' INT
trap 'cleanup; exit 143' TERM

# target/ in a named volume rather than on the bind mount, and the same volume the `dev` compose
# service uses. Two reasons. Rust build I/O through virtiofs dominates a rebuild on macOS, and
# without this the repository carries two build directories for one checkout: this one on the host
# and the dev loop's inside Docker.
#
# It does not halve anything. `check` and `test` build the dev profile and the dev loop builds
# `dev-release`, and cargo keeps those in separate subdirectories, so the dependency tree still
# compiles once for each. What is shared is the volume, the registry and the fingerprint database.
#
# The cost: a `test` run while the dev loop is up blocks on cargo's build-directory lock until the
# dev loop's build finishes. That is cargo serialising two real builds rather than the stale
# container problem above, and it clears on its own.
#
# target/ no longer appears on the host. `docker run --rm -v lumberroom-target:/t alpine ls /t` reads it.
docker run --rm --name "$NAME" \
  --label "lumberroom.cargo.owner=$$" \
  --network "${LUMBERROOM_DOCKER_NETWORK:-lumberroom_default}" \
  -v "$PWD:/app" \
  -v lumberroom-target:/app/target \
  -v lumberroom-cargo:/usr/local/cargo/registry \
  -e DATABASE_URL="postgres://${POSTGRES_USER:-lumberroom}:${POSTGRES_PASSWORD}@db:5432/${POSTGRES_DB:-lumberroom}" \
  -e CARGO_TERM_COLOR=never \
  -e RUST_BACKTRACE=1 \
  lumberroom-builder cargo "$@"
status=$?
exit "$status"
