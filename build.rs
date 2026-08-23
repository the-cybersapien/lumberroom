// The build stamp is baked in at compile time and read back with `option_env!`. Cargo does not
// notice an environment change on its own, so without these lines a rebuild keeps yesterday's
// stamp, which is the failure `src/build_info.rs` exists to catch.
fn main() {
    println!("cargo:rerun-if-env-changed=LUMBERROOM_BUILD_SHA");
    println!("cargo:rerun-if-env-changed=LUMBERROOM_BUILD_TAG");
    println!("cargo:rerun-if-env-changed=LUMBERROOM_BUILT_AT");
}
