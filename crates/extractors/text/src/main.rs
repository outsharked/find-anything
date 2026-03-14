use find_extract_types::{run::{init_tracing, run_extractor}, ExtractorConfig};

fn main() {
    init_tracing("warn");
    run_extractor(|path, args| {
        if !find_extract_text::accepts(path) {
            return Ok(vec![]);
        }
        let cfg = ExtractorConfig {
            max_content_kb: args.first().and_then(|s| s.parse().ok()).unwrap_or(10240),
            ..Default::default()
        };
        find_extract_text::extract(path, &cfg)
    });
}
