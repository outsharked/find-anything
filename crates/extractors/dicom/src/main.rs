use find_extract_types::{
    run::{init_tracing, run_extractor},
    ExtractorConfig,
};

fn main() {
    init_tracing("warn");
    run_extractor(|path, _args| {
        find_extract_dicom::extract(path, &ExtractorConfig::default())
    });
}
