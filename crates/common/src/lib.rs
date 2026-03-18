pub mod api;
pub mod config;
pub mod logging;
pub mod mem;
pub mod path;
pub mod subprocess;

pub use find_extract_types::build_globset;

/// Git commit hash at build time, injected via `GIT_HASH` env var by the mise build tasks.
/// Falls back to `"unknown"` for raw `cargo build` invocations.
pub const GIT_HASH: &str = match option_env!("GIT_HASH") { Some(h) => h, None => "unknown" };
/// Git tag at HEAD at build time (empty string if none).
pub const GIT_TAG: &str  = match option_env!("GIT_TAG")  { Some(t) => t, None => "" };
/// Non-empty (`"1"`) when the working tree had uncommitted changes at build time.
pub const GIT_DIRTY: &str = match option_env!("GIT_DIRTY") { Some(d) => d, None => "" };

/// Returns the tool version string for `--version` output.
///
/// On a clean release tag build: `"0.7.0"`
/// On a dirty working tree:      `"0.7.0 (abc1234+)"`
/// Otherwise (post-release/dev): `"0.7.0 (abc1234)"`
///
/// Git info is captured by `find-common`'s `build.rs` and exposed as
/// `find_common::{GIT_HASH, GIT_TAG, GIT_DIRTY}`.
#[macro_export]
macro_rules! tool_version {
    () => {{
        let pkg_version = env!("CARGO_PKG_VERSION");
        let hash  = $crate::GIT_HASH;
        let tag   = $crate::GIT_TAG;
        let dirty = $crate::GIT_DIRTY;
        // clap's Command::version() requires &'static str; Box::leak is fine for a
        // one-time startup allocation.
        if !dirty.is_empty() {
            Box::leak(format!("{} ({hash}+)", pkg_version).into_boxed_str()) as &str
        } else if tag == format!("v{}", pkg_version) {
            pkg_version
        } else {
            Box::leak(format!("{} ({})", pkg_version, hash).into_boxed_str()) as &str
        }
    }};
}
