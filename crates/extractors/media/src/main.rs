use find_extract_types::{run::{init_tracing, run_extractor}, ExtractorConfig};

fn main() {
    init_tracing("warn");
    run_extractor(|path, args| {
        let cfg = ExtractorConfig {
            max_content_kb: args.first().and_then(|s| s.parse().ok()).unwrap_or(102400),
            ..Default::default()
        };
        find_extract_media::extract(path, &cfg)
    });
}
