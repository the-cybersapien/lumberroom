# 12. Distribute lumberroom as four raw binaries off a git tag, built from two places

**Date:** 22 August 2026 · **Status:** accepted, scripted; the darwin leg unverified · **Decided
by:** the owner

## Decision

A tag `vX.Y.Z` produces a GitHub release on the repo carrying four binaries and one
checksum file:

```
lumberroom-X.Y.Z-aarch64-unknown-linux-musl
lumberroom-X.Y.Z-x86_64-unknown-linux-musl
lumberroom-X.Y.Z-aarch64-apple-darwin
lumberroom-X.Y.Z-x86_64-apple-darwin
SHA256SUMS
```

The two linux legs come from `scripts/cli-release.sh`, run against the existing `lumberroom-builder`
image with no new toolchain on this machine; it only builds and checksums, and never tags, pushes,
or publishes. The two darwin legs come from `.github/workflows/cli-release.yml`, a `macos-14`
runner, because darwin has no cross-compiler in the container this repository already builds in.
The workflow does not rebuild the linux legs: `scripts/cli-release.sh` already covers them, and a
second CI job producing the same two binaries a third time would be paying for coverage that
already exists. It is the one piece of this decision that calls `gh release`, and only because a
tag push is itself the deliberate, human-initiated act: it creates the release if the tag has none
yet, downloads whatever is already attached (the linux legs, if a local
`cli-release.sh --build` run has already uploaded them), and re-checksums everything present so
`SHA256SUMS` always covers every asset on the release rather than only the two this job built,
regardless of which half runs first.

Raw binaries, not archives. Each target is one static or self-contained executable of 5 to 6MB
with no man page and no library beside it, so a tarball would add a step and carry nothing.

## The context that forced it

`crates/lumberroom` reads `~/.claude/projects` and `~/.codex/sessions`, which live on the owner's
Mac. `scripts/lumberroom.sh` runs it today by mounting those host paths into a linux container at
their real paths and forcing `HOME` to match, because the ingest watermark keys on `file_path`: a
container path and the real path are two different files to it. A native darwin binary removes
that whole mount-and-rename dance. It is the artifact that matters most, and it is the one target
the `lumberroom-builder` container cannot produce, since there is no osxcross and no zig in it and no
local Rust toolchain on the host to fall back on.

TLS settled itself while scoping this: `reqwest`'s `rustls-tls` feature resolves to the **ring**
provider, not `aws-lc-rs` (`aws-lc` appears zero times in `Cargo.lock`). ring needs a C compiler and
no cmake, and `webpki-roots` ships Mozilla's roots in the binary, so no target here needs a system
CA store or a build tool beyond `cc`. That is what makes the linux musl legs buildable without
installing a cross toolchain from source: `musl-tools` and a foreign `gcc` package are enough.

## What lost, and why

**A host rustup install, rejected for the release path itself.** Xcode 26.6 is already on the Mac
that would run it, so `rustup target add x86_64-apple-darwin` would give both darwin slices from
one machine with no extra toolchain. It was the cheapest option and it lost anyway: it would make
the release artifact depend on whatever rustc happens to be on a laptop that day, with no record of
which, and it breaks the property that every build here goes through a named, versioned image. CI
pays a real cost to keep that property; `scripts/cli-release.sh` still opens the door back to a
host toolchain, on purpose, in its darwin branch, gated on `command -v cargo`, because closing that
door would make the release depend on CI being reachable at all.

**`cargo-zigbuild` cross-compiling darwin from the linux container, deferred rather than rejected.**
It would remove the macOS runner and its cost entirely, mounting the Xcode SDK already on this
machine as `SDKROOT`. Nobody has run it. `crates/lumberroom` links no Apple framework beyond
libSystem, rustls rather than native-tls, no keychain, which is the shape zig cross-compiles best,
so it is the first thing to try if the CI cost below becomes a problem. Recorded as the reversal
path, not built, because building it untested and shipping the first release from it would be
testing zigbuild and the release process at once.

**A universal darwin binary via `lipo -create`.** One binary instead of two would be nicer to
install. It is one extra line once both slices exist and was left out because it forces a choice
between shipping the fat binary alone (a user who wants to check what target they are running
cannot, from the file) or shipping it alongside the two slices (three darwin artifacts instead of
two, for a convenience nobody has asked for yet).

**Windows, dropped from this release entirely.** `crates/lumberroom/src/config.rs` resolves the
config path from `HOME` with no `USERPROFILE` fallback, and `restrict()`, which chmods the token
file to 0600 on unix, is a no-op everywhere else: a Windows build today would write a bearer token
to a file with no permission narrowing and print no warning that it had not. `ingest/mod.rs` fails
outright with no state directory. Shipping a Windows binary before those three lines change would
ship a credential-handling regression under a release tag, which reads as tested. Fix `config.rs`
and `ingest/mod.rs` first; then `x86_64-pc-windows-gnu` is a cross-compile from the existing
container with mingw-w64, and it can join the same two files this decision already touches.

## Costs accepted

**`macos-14` runners bill at ten times the linux rate.** A five-minute darwin
job costs roughly fifty linux-equivalent minutes against the account's Actions quota, on every tag
pushed, and there is no linux job in the same workflow to make that cost look small by comparison:
the whole workflow is the darwin build. This is a real recurring cost, accepted because the
alternative inside this decision, a host toolchain, was rejected above for a reason that still
holds, and the alternative outside it, zigbuild, is unverified.

**`scripts/cli-release.sh` reinstalls the linux cross toolchain on every run**, rather than it
living in `Dockerfile.builder`. That file belongs to another owner this round; the wire-in is on
record for whoever picks it up. Until then every release pays the `apt-get install` cost again:
the scout's run measured 188s total for the apt install plus both builds (59.1s and 68.4s), so
roughly 60s of that is the repeated toolchain install, and that is not the expensive half of this
decision.

**The release depends on two runs landing, in either order.** The linux legs come from a local
`cli-release.sh --build` plus a manual `gh release create` or `gh release upload`; the darwin legs
come from the tag push triggering CI. Nothing here enforces that both happen. A tag pushed with no
local upload first, or ever, produces a release carrying only the two darwin binaries and a
`SHA256SUMS` that only covers those two, silently short two targets. The workflow re-checksums
whatever it finds rather than asserting four assets are present, so this is a gap a release could
ship, not one that fails loudly.

**One release, two build environments.** The linux binaries in a given release come from whatever
`lumberroom-builder` image is tagged `lumberroom-builder` locally when `cli-release.sh --build` runs, and the
darwin binaries come from whatever `macos-14` resolves to that day on GitHub's side. Neither is
pinned to a recorded digest yet. A release built partly on a laptop and partly in CI is only as
reproducible as `Cargo.lock`, which is why `--locked` is on every build command in both places and
is the one thing standing in for full reproducibility until an image digest is recorded per
release.

**No anonymous install line.** Distribution is `gh release download`, which reaches people
with repo access and nobody else. That matches who this client is for: it reads the owner's own
transcripts. `README.md` carries no install section for `lumberroom` yet; whoever adds one should
write `gh`, not `curl | sh`, until there is a reason to.

**The binary cannot say what it is.** `lumberroom` has no `version` command and no `--version` flag,
so a downloaded asset and an already-installed copy cannot be told apart by running either of them.
This decision ships anyway, because the artifact naming (`lumberroom-X.Y.Z-<target>`) and
`SHA256SUMS` carry that information externally for the first release. Giving the crate a `version`
arm and a `build.rs` stamp matching the server's is follow-up work, not a blocker, and it is not
done by either file this decision owns.

## What this is explicitly not for

**Tagging or pushing.** Neither `scripts/cli-release.sh` nor the CI workflow runs `git tag` or
`git push`. `cli-release.sh` builds and checksums, full stop; nothing in it can create a release,
because it has no `gh` call at all. The workflow's `gh release create`/`gh release upload` run only
because a tag has already reached the remote, which is the deliberate, human-initiated act this
whole pipeline reacts to rather than performs.

**A general cross-compilation story for the server crate.** Everything here is scoped to
`-p lumberroom`, which has no path dependency on the root crate and never touches `fastembed` or
ONNX Runtime. The server stays container-built, one platform, and this decision says nothing about
changing that.

**Making Windows work.** It names exactly what blocks it and stops there.

**A reproducible-builds guarantee.** `--locked` pins dependency versions; it does not pin the
compiler, the base image digest, or the runner image GitHub resolves `macos-14` to on a given day.
Treat "reproducible" as future work, not a property this release already has.

## The reversal condition

If the macOS Actions bill becomes a real line item, or if the owner installs rustup on the Mac
before that happens, try `cargo-zigbuild` against the local Xcode SDK first; it removes the runner
cost without reintroducing the host-toolchain dependency the first rejection above was written
against. If zigbuild cannot produce a darwin binary rustls and this client's dependency graph are
happy with, fall back to the host-toolchain branch `scripts/cli-release.sh` already has, and record
the rustc version used in the release notes since nothing else will.

If a Windows user is asked for, `config.rs` and `ingest/mod.rs` change first, in that order, and
only then does a third job join this workflow.
