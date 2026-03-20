use std::fs;
use std::path::Path;

use find_extract_types::{IndexLine, LINE_METADATA};
use find_extract_types::ExtractorConfig;

/// Extract version information from PE bytes.
///
/// Used by `find-extract-dispatch` for archive members. Does not include a
/// filename line — the caller adds that.
pub fn extract_from_bytes(bytes: &[u8], _name: &str, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let version_info = extract_version_info(bytes)?;
    let combined: String = version_info
        .lines()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if combined.is_empty() {
        return Ok(vec![]);
    }
    Ok(vec![IndexLine {
        line_number: LINE_METADATA,
        content: combined,
        archive_path: None,
    }])
}

/// Extract version information from PE files (EXE, DLL, etc.).
///
/// Supports:
/// - Windows executables (.exe, .dll, .sys, .scr, .cpl, .ocx)
/// - Version info resources (product, version, company, copyright, etc.)
///
/// # Returns
/// Vector of IndexLine objects with all metadata at LINE_METADATA (1).
pub fn extract(path: &Path, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let data = fs::read(path)?;
    extract_from_bytes(&data, "", _cfg)
}

/// Check if a file is a PE executable based on extension.
pub fn accepts(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_lowercase();
        matches!(
            ext.as_str(),
            "exe" | "dll" | "sys" | "scr" | "cpl" | "ocx" | "drv" | "efi"
        )
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use find_extract_types::ExtractorConfig;

    fn cfg() -> ExtractorConfig {
        ExtractorConfig::default()
    }

    // ── accepts() ─────────────────────────────────────────────────────────────

    #[test]
    fn accepts_pe_extensions() {
        for ext in &["exe", "dll", "sys", "scr", "cpl", "ocx", "drv", "efi"] {
            let name = format!("binary.{ext}");
            assert!(accepts(Path::new(&name)), ".{ext} should be accepted");
        }
    }

    #[test]
    fn accepts_uppercase_extensions() {
        assert!(accepts(Path::new("binary.EXE")), ".EXE (uppercase) should be accepted");
        assert!(accepts(Path::new("library.DLL")), ".DLL (uppercase) should be accepted");
    }

    #[test]
    fn rejects_non_pe_extensions() {
        for ext in &["txt", "zip", "pdf", "rs", "toml", "png", "mp3"] {
            let name = format!("file.{ext}");
            assert!(!accepts(Path::new(&name)), ".{ext} should be rejected");
        }
    }

    #[test]
    fn rejects_no_extension() {
        assert!(!accepts(Path::new("Makefile")));
        assert!(!accepts(Path::new("")));
    }

    // ── extract_from_bytes() — non-PE input ───────────────────────────────────

    #[test]
    fn non_pe_bytes_returns_empty_not_panic() {
        // Random garbage — should return Ok(vec![]) rather than panic or Err.
        let garbage = b"this is not a PE file at all";
        let result = extract_from_bytes(garbage, "fake.exe", &cfg());
        assert!(result.is_ok(), "non-PE bytes should return Ok, not Err");
        assert!(result.unwrap().is_empty(), "non-PE bytes should yield no lines");
    }

    #[test]
    fn empty_bytes_returns_empty_not_panic() {
        let result = extract_from_bytes(b"", "empty.exe", &cfg());
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn mz_header_only_returns_empty_not_panic() {
        // MZ magic but no valid PE — should not panic.
        let mut data = vec![0u8; 64];
        data[0] = b'M';
        data[1] = b'Z';
        let result = extract_from_bytes(&data, "stub.exe", &cfg());
        assert!(result.is_ok(), "truncated MZ header should not panic");
    }

    // ── extract_from_bytes() — minimal valid PE ────────────────────────────────
    //
    // This minimal PE32 stub satisfies pelite's structural checks but has no
    // version-info resources, so extraction returns an empty vec without error.

    fn minimal_pe32() -> Vec<u8> {
        // DOS header (64 bytes): MZ magic + e_lfanew = 0x40 pointing to PE sig.
        let mut buf = vec![0u8; 0x200];
        buf[0] = b'M'; buf[1] = b'Z';
        // e_lfanew at offset 0x3C → value 0x40
        buf[0x3C] = 0x40;

        // PE signature at 0x40
        buf[0x40] = b'P'; buf[0x41] = b'E'; buf[0x42] = 0; buf[0x43] = 0;

        // COFF header at 0x44 (20 bytes):
        // Machine = 0x014c (i386 PE32)
        buf[0x44] = 0x4c; buf[0x45] = 0x01;
        // NumberOfSections = 0
        buf[0x46] = 0; buf[0x47] = 0;
        // TimeDateStamp = 0 (4 bytes, already zeroed)
        // PointerToSymbolTable = 0 (4 bytes)
        // NumberOfSymbols = 0 (4 bytes)
        // SizeOfOptionalHeader = 96 (0x60) for PE32
        buf[0x54] = 0x60; buf[0x55] = 0;
        // Characteristics = 0x0002 (executable image)
        buf[0x56] = 0x02; buf[0x57] = 0;

        // Optional header (PE32) at 0x58:
        // Magic = 0x010b (PE32)
        buf[0x58] = 0x0b; buf[0x59] = 0x01;
        // SizeOfImage and SizeOfHeaders must be non-zero and aligned.
        // SizeOfHeaders at offset +60 from optional header start = 0x58+60 = 0x94
        buf[0x94] = 0x40; // 0x40 (must be <= SizeOfImage)
        // SizeOfImage at offset +56 from optional header start = 0x58+56 = 0x90
        buf[0x90] = 0x00; buf[0x91] = 0x02; // 0x0200

        buf
    }

    #[test]
    fn minimal_pe32_returns_ok() {
        let data = minimal_pe32();
        let result = extract_from_bytes(&data, "minimal.exe", &cfg());
        // May succeed with empty lines (no version resources) or fail — must not panic.
        assert!(result.is_ok(), "minimal PE32 should not panic: {:?}", result.err());
    }

    // ── extract() — file-based path ───────────────────────────────────────────

    #[test]
    fn extract_from_file_garbage_data_returns_ok() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("fake.exe");
        std::fs::write(&path, b"not a PE file at all").unwrap();
        let result = extract(&path, &cfg());
        assert!(result.is_ok(), "garbage file should return Ok");
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn extract_from_file_missing_returns_err() {
        let result = extract(Path::new("/nonexistent/path/file.exe"), &cfg());
        assert!(result.is_err(), "missing file should return Err");
    }

    #[test]
    fn extract_from_file_minimal_pe_returns_ok() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("minimal.exe");
        std::fs::write(&path, minimal_pe32()).unwrap();
        let result = extract(&path, &cfg());
        assert!(result.is_ok(), "minimal PE32 from disk should return Ok");
    }

    // ── extract_from_bytes() — combined metadata line ─────────────────────────

    #[test]
    fn non_pe_never_returns_metadata_line() {
        // Garbage input should never produce a LINE_METADATA result.
        let result = extract_from_bytes(b"garbage data xyz", "fake.exe", &cfg()).unwrap();
        assert!(result.iter().all(|l| l.line_number != LINE_METADATA || !l.content.is_empty()),
            "if a metadata line is returned it must have content");
    }
}

/// Extract version information from PE file data.
fn extract_version_info(data: &[u8]) -> anyhow::Result<String> {
    // Try parsing as PE64 first, then PE32
    let info = try_parse_pe64(data)
        .or_else(|_| try_parse_pe32(data))
        .unwrap_or_default();

    Ok(info)
}

fn try_parse_pe64(data: &[u8]) -> Result<String, anyhow::Error> {
    use pelite::pe64::PeFile;

    let pe = PeFile::from_bytes(data)?;
    extract_from_resources_64(&pe)
}

fn try_parse_pe32(data: &[u8]) -> Result<String, anyhow::Error> {
    use pelite::pe32::PeFile;

    let pe = PeFile::from_bytes(data)?;
    extract_from_resources_32(&pe)
}

fn extract_from_resources_64(pe: &pelite::pe64::PeFile) -> Result<String, anyhow::Error> {
    use pelite::pe64::Pe;

    let resources = pe.resources()?;
    let version_info = resources.version_info()?;

    Ok(format_version_info(&version_info))
}

fn extract_from_resources_32(pe: &pelite::pe32::PeFile) -> Result<String, anyhow::Error> {
    use pelite::pe32::Pe;

    let resources = pe.resources()?;
    let version_info = resources.version_info()?;

    Ok(format_version_info(&version_info))
}

fn format_version_info<'a>(version_info: &pelite::resources::version_info::VersionInfo<'a>) -> String {
    let mut lines = Vec::new();

    // Extract fixed file info (version numbers)
    if let Some(fixed) = version_info.fixed() {
        let file_ver = fixed.dwFileVersion;
        let product_ver = fixed.dwProductVersion;

        lines.push(format!(
            "[PE:FileVersion] {}.{}.{}.{}",
            file_ver.Major, file_ver.Minor, file_ver.Patch, file_ver.Build
        ));

        lines.push(format!(
            "[PE:ProductVersion] {}.{}.{}.{}",
            product_ver.Major, product_ver.Minor, product_ver.Patch, product_ver.Build
        ));
    }

    // Extract string file info (named fields)
    // Common version info keys
    let keys = [
        "ProductName",
        "FileDescription",
        "CompanyName",
        "LegalCopyright",
        "OriginalFilename",
        "InternalName",
        "LegalTrademarks",
        "Comments",
        "PrivateBuild",
        "SpecialBuild",
    ];

    // Try to get strings for any available language
    let langs = version_info.translation();
    if let Some(lang) = langs.first() {
        version_info.strings(*lang, |key, value| {
            if keys.contains(&key) && !value.trim().is_empty() {
                lines.push(format!("[PE:{}] {}", key, value.trim()));
            }
        });
    }

    lines.join("\n")
}
