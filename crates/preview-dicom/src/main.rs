use std::io::{self, Write as _};
use std::path::Path;
use std::process;

use dicom_pixeldata::{ConvertOptions, PixelDecoder, VoiLutOption};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: find-preview-dicom <file-path>");
        process::exit(1);
    }

    let path = Path::new(&args[1]);

    if let Err(e) = run(path) {
        eprintln!("find-preview-dicom: {}: {}", path.display(), e);
        process::exit(1);
    }
}

fn run(path: &Path) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let obj = dicom_object::open_file(path)
        .with_context(|| format!("failed to open DICOM file: {}", path.display()))?;

    let pixel_data = obj
        .decode_pixel_data()
        .context("failed to decode pixel data")?;

    let options = ConvertOptions::new()
        .with_voi_lut(VoiLutOption::Default)
        .force_8bit();

    let image = pixel_data
        .to_dynamic_image_with_options(0, &options)
        .context("failed to convert pixel data to image")?;

    // Encode to PNG in memory, then stream to stdout.
    // (image::write_to requires Seek, which stdout does not implement.)
    let mut buf = Vec::new();
    image
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .context("failed to encode PNG")?;

    io::stdout().lock().write_all(&buf)?;
    Ok(())
}
