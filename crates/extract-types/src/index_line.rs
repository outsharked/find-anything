use serde::{Deserialize, Serialize};

/// Version of the scanner/extraction logic. Stored with each indexed file so
/// that `find-scan --upgrade` can selectively re-index files that were indexed
/// by an older version of the client. Increment this when extraction logic
/// changes in a way that produces meaningfully different output.
pub const SCANNER_VERSION: u32 = 1;

/// A single extracted line sent from client → server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexLine {
    /// NULL for regular files; inner path for archive entries; "page:N" for PDFs.
    pub archive_path: Option<String>,
    pub line_number: usize,
    pub content: String,
}

/// Classify a file by its extension alone — no extractor lib deps.
/// Used by `find-watch` (subprocess mode) and `batch.rs` for archive member kinds.
///
/// Returns `"unknown"` for extensions not in any known category.  Callers that
/// also run content extraction (scan.rs, batch.rs) refine "unknown" to "text"
/// or "binary" based on the actual bytes.
pub fn detect_kind_from_ext(ext: &str) -> &'static str {
    match ext.to_lowercase().as_str() {
        "zip" | "tar" | "gz" | "bz2" | "xz" | "tgz" | "tbz2" | "txz" | "7z" => "archive",
        "pdf" => "pdf",
        "jpg" | "jpeg" | "png" | "gif" | "bmp" | "ico" | "webp" | "heic"
        | "tiff" | "tif" | "raw" | "cr2" | "nef" | "arw" => "image",
        "mp3" | "flac" | "ogg" | "m4a" | "aac" | "wav" | "wma" | "opus" => "audio",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "m4v" | "flv" => "video",
        "docx" | "xlsx" | "xls" | "xlsm" | "pptx" | "epub" => "document",
        // Known binary formats
        "exe" | "dll" | "so" | "dylib" | "sys" | "scr" | "efi"
        | "o" | "a" | "lib" | "obj" | "wasm"
        | "deb" | "rpm" | "pkg" | "msi" | "snap" | "flatpak"
        | "class" | "jar" | "pyc" | "pyd"
        | "bin" | "img" | "iso" | "dmg" | "vmdk" | "vhd" | "qcow2"
        | "db" | "sqlite" | "sqlite3" | "mdb"
        | "ttf" | "otf" | "woff" | "woff2"
        => "binary",
        // Known text formats — we are confident these are human-readable
        "rs" | "ts" | "js" | "mjs" | "cjs" | "jsx" | "tsx"
        | "py" | "rb" | "go" | "java" | "c" | "cpp" | "cc" | "cxx" | "h" | "hpp"
        | "cs" | "swift" | "kt" | "scala" | "r" | "m" | "pl"
        | "sh" | "bash" | "zsh" | "fish" | "ps1" | "bat" | "cmd" | "vbs" | "vba" | "bas" | "cls"
        | "lua" | "el" | "clj" | "hs" | "ml" | "fs" | "ex" | "erl"
        | "dart" | "jl" | "nim" | "zig" | "s" | "asm"
        | "html" | "htm" | "xhtml" | "xml" | "svg" | "css" | "scss" | "sass" | "less"
        | "json" | "yaml" | "yml" | "toml" | "ini" | "cfg" | "conf" | "env"
        | "properties" | "plist" | "nix" | "hcl" | "tf"
        | "csv" | "tsv" | "sql" | "graphql" | "gql" | "proto"
        | "md" | "markdown" | "rst" | "tex" | "adoc" | "org"
        | "txt" | "log" | "diff" | "patch" | "lock"
        | "gitignore" | "gitattributes" | "gitmodules" | "dockerignore"
        | "makefile" | "dockerfile" | "procfile" | "gemfile" | "rakefile"
        | "mod" | "sum" | "cabal" | "gradle" | "sln" | "csproj" | "vcxproj"
        => "text",
        // Everything else: don't guess — let content inspection decide
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_kind_archives() {
        for ext in &["zip", "tar", "gz", "bz2", "xz", "tgz", "tbz2", "txz", "7z"] {
            assert_eq!(detect_kind_from_ext(ext), "archive", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_pdf() {
        assert_eq!(detect_kind_from_ext("pdf"), "pdf");
    }

    #[test]
    fn test_detect_kind_images() {
        for ext in &["jpg", "jpeg", "png", "gif", "bmp", "ico", "webp", "heic",
                     "tiff", "tif", "raw", "cr2", "nef", "arw"] {
            assert_eq!(detect_kind_from_ext(ext), "image", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_audio() {
        for ext in &["mp3", "flac", "ogg", "m4a", "aac", "wav", "wma", "opus"] {
            assert_eq!(detect_kind_from_ext(ext), "audio", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_video() {
        for ext in &["mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "flv"] {
            assert_eq!(detect_kind_from_ext(ext), "video", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_known_text_exts() {
        for ext in &["rs", "py", "toml", "md", "txt", "json"] {
            assert_eq!(detect_kind_from_ext(ext), "text", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_unknown_ext_returns_unknown() {
        for ext in &["", "unknown", "xyz", "foobar"] {
            assert_eq!(detect_kind_from_ext(ext), "unknown", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_documents() {
        for ext in &["docx", "xlsx", "xls", "xlsm", "pptx", "epub"] {
            assert_eq!(detect_kind_from_ext(ext), "document", "ext={ext}");
        }
    }

    #[test]
    fn test_detect_kind_case_insensitive() {
        assert_eq!(detect_kind_from_ext("PDF"), "pdf");
        assert_eq!(detect_kind_from_ext("ZIP"), "archive");
        assert_eq!(detect_kind_from_ext("JPG"), "image");
        assert_eq!(detect_kind_from_ext("MP3"), "audio");
        assert_eq!(detect_kind_from_ext("MP4"), "video");
        assert_eq!(detect_kind_from_ext("DOCX"), "document");
    }
}
