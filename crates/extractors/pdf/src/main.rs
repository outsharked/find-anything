use find_extract_types::{run::{init_tracing, run_extractor}, ExtractorConfig};

fn main() {
    init_tracing("warn,lopdf=off");
    run_extractor(|path, args| {
        let cfg = ExtractorConfig {
            max_content_kb: args.first().and_then(|s| s.parse().ok()).unwrap_or(102400),
            max_line_length: args.get(1).and_then(|s| s.parse().ok()).unwrap_or(120),
            ..Default::default()
        };
        find_extract_pdf::extract(path, &cfg)
    });
}
