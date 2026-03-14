use std::path::Path;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

use crate::IndexLine;

/// Initialise tracing for an extractor subprocess.
///
/// Log output goes to stderr; timestamps and ANSI colours are disabled so the
/// output is clean when captured by the parent process.
///
/// `log_filter` is the fallback directive when `RUST_LOG` is not set
/// (e.g. `"warn"` or `"warn,lopdf=off"`).
pub fn init_tracing(log_filter: &str) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| log_filter.parse().unwrap_or_else(|_| "warn".parse().unwrap()));
    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .without_time()
                .with_ansi(false),
        )
        .init();
}

/// Run a standard (non-streaming) extractor subprocess.
///
/// Parses `args[1]` as the file path, passes `args[2..]` to the `extract`
/// closure as extra arguments (for config values like `max_content_kb`),
/// serialises the returned lines to compact JSON on stdout, and exits.
///
/// This function never returns — it always calls `std::process::exit`.
pub fn run_extractor<F>(extract: F) -> !
where
    F: FnOnce(&Path, &[String]) -> anyhow::Result<Vec<IndexLine>>,
{
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <file-path> [options]", args[0]);
        std::process::exit(1);
    }

    let path = Path::new(&args[1]);
    let extra = &args[2..];

    match extract(path, extra) {
        Ok(lines) => match serde_json::to_string(&lines) {
            Ok(json) => {
                println!("{json}");
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("serialization error: {e}");
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("extraction error for {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}
