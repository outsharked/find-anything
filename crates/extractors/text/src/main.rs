use std::path::Path;
use std::process;
use find_extract_types::ExtractorConfig;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

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
        eprintln!("Usage: find-extract-text <file-path>");
        eprintln!();
        eprintln!("Extracts text content from files and outputs JSON.");
        eprintln!();
        eprintln!("Supported formats:");
        eprintln!("  - Plain text files");
        eprintln!("  - Source code (Rust, Python, JavaScript, etc.)");
        eprintln!("  - Markdown (with frontmatter extraction)");
        eprintln!("  - Config files (JSON, YAML, TOML, etc.)");
        process::exit(1);
    }

    let path = Path::new(&args[1]);
    let cfg = ExtractorConfig {
        max_content_kb: args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10240),
        max_depth: 10,
        max_line_length: 120,
        ..Default::default()
    };

    if !find_extract_text::accepts(path) {
        println!("[]");
        process::exit(0);
    }

    match find_extract_text::extract(path, &cfg) {
        Ok(lines) => {
            // Output JSON to stdout
            match serde_json::to_string_pretty(&lines) {
                Ok(json) => {
                    println!("{}", json);
                    process::exit(0);
                }
                Err(e) => {
                    eprintln!("Error serializing to JSON: {}", e);
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("Error extracting text from {}: {}", path.display(), e);
            process::exit(1);
        }
    }
}
