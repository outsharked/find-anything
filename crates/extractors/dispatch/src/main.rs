use std::path::Path;
use std::process;

use find_extract_types::ExtractorConfig;
use tracing_subscriber::{layer::SubscriberExt as _, util::SubscriberInitExt as _};

fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "warn".into()))
        .with(tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .without_time()
            .with_ansi(false))
        .init();


    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: find-extract-dispatch <file-path> [max-content-kb] [max-line-length]");
        process::exit(1);
    }

    let path = Path::new(&args[1]);
    let cfg = ExtractorConfig {
        max_content_kb: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10240),
        max_line_length: args.get(3).and_then(|s| s.parse().ok()).unwrap_or(120),
        ..Default::default()
    };

    match find_extract_dispatch::dispatch_from_path(path, &cfg) {
        Ok(lines) => {
            match serde_json::to_string(&lines) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("serialization error: {e}");
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("extraction error for {}: {e}", path.display());
            process::exit(1);
        }
    }
}
