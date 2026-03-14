pub mod api;
pub mod config;
pub mod logging;
pub mod mem;
pub mod path;
pub mod subprocess;

pub use find_extract_types::build_globset;

/// Returns the tool version string for `--version` output.
///
/// On a clean release tag build: `"0.7.0"`
/// On a dirty working tree:      `"0.7.0 (dev)"`
/// Otherwise (post-release/dev): `"0.7.0 (abc1234)"`
///
/// The `option_env!` calls expand at **the calling crate's** compile time,
/// so each binary embeds its own build-time constants — no `build.rs` needed.
#[macro_export]
macro_rules! tool_version {
    () => {{
        let pkg_version = env!("CARGO_PKG_VERSION");
        let hash  = option_env!("GIT_HASH").unwrap_or("unknown");
        let tag   = option_env!("GIT_TAG").unwrap_or("").trim();
        let dirty = option_env!("GIT_DIRTY").unwrap_or("").trim();
        // clap's Command::version() requires &'static str; Box::leak is fine for a
        // one-time startup allocation.
        if !dirty.is_empty() {
            Box::leak(format!("{} (dev)", pkg_version).into_boxed_str()) as &str
        } else if tag == format!("v{}", pkg_version) {
            pkg_version
        } else {
            Box::leak(format!("{} ({})", pkg_version, hash).into_boxed_str()) as &str
        }
    }};
}
