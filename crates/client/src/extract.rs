use std::path::Path;

/// Detect the file kind string used in IndexFile.kind.
pub fn detect_kind(path: &Path) -> &'static str {
    if find_extract_archive::accepts(path) {
        return "archive";
    }
    if find_extract_pdf::accepts(path) {
        return "pdf";
    }
    if find_extract_pe::accepts(path) {
        return "executable";
    }
    if find_extract_dicom::accepts(path) {
        return "dicom";
    }
    if find_extract_media::accepts(path) {
        // Determine if it's image, audio, or video
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if find_extract_media::is_image_ext(&ext) {
            return "image";
        }
        if find_extract_media::is_audio_ext(&ext) {
            return "audio";
        }
        if find_extract_media::is_video_ext(&ext) {
            return "video";
        }
    }
    "unknown"
}
