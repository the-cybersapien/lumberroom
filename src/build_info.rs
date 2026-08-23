//! What this binary was built from.
//!
//! `docker restart` reuses a container's original image and so does `docker compose up -d` in some
//! cases, so a rebuilt image can sit on disk while the old binary keeps serving every request.
//! Nothing in the product said so, and it cost two debugging sessions in one day. These constants
//! travel with the binary, `/readyz` reports them, and `scripts/deploy-check.sh` compares them
//! against what the caller built.
//!
//! They are compile-time, not configuration. Reading them from the environment at boot would let a
//! container claim a commit it was not built from, which is the whole failure.

/// Short commit the binary was built from. `unknown` when the build passed nothing, which is what a
/// plain `cargo build` outside the Dockerfile does.
pub const SHA: &str = match option_env!("LUMBERROOM_BUILD_SHA") {
    // An explicitly empty variable reads as "not stamped", not as a commit named "".
    Some(s) if !s.is_empty() => s,
    _ => "unknown",
};

/// Image tag the build was stamped with, `unknown` outside an image build.
pub const TAG: &str = match option_env!("LUMBERROOM_BUILD_TAG") {
    // An explicitly empty variable reads as "not stamped", not as a commit named "".
    Some(s) if !s.is_empty() => s,
    _ => "unknown",
};

/// RFC 3339 instant the build started, `unknown` when the build passed nothing.
pub const BUILT_AT: &str = match option_env!("LUMBERROOM_BUILT_AT") {
    // An explicitly empty variable reads as "not stamped", not as a commit named "".
    Some(s) if !s.is_empty() => s,
    _ => "unknown",
};
