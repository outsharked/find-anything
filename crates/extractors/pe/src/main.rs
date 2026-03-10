use std::io::{self, BufRead};
use std::path::Path;
use find_extract_types::ExtractorConfig;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "warn".into()))
        .with(tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .without_time()
            .with_ansi(false))
        .init();

    let cfg = ExtractorConfig {
        max_content_kb: 100 * 1024,
        max_depth: 10,
        max_line_length: 120,
        ..Default::default()
    };

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let path_str = line?;
        let path = Path::new(&path_str);

        if !find_extract_pe::accepts(path) {
            continue;
        }

        match find_extract_pe::extract(path, &cfg) {
            Ok(lines) => {
                for index_line in lines {
                    println!("{}", serde_json::to_string(&index_line)?);
                }
            }
            Err(e) => {
                eprintln!("Error extracting {}: {}", path_str, e);
            }
        }
    }

    Ok(())
}
