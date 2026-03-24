use find_extract_types::{run::{init_tracing, run_extractor}, ExtractorConfig};

fn main() {
    init_tracing("warn,nom_exif=off");
    run_extractor(|path, args| {
        // args[0] = max_content_kb, args[1] = ffprobe_path (empty string = disabled)
        let ffprobe_path = args.get(1)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned());
        let cfg = ExtractorConfig {
            max_content_kb: args.first().and_then(|s| s.parse().ok()).unwrap_or(102400),
            ffprobe_path,
            ..Default::default()
        };
        find_extract_media::extract(path, &cfg)
    });
}
