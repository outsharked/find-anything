use std::fs;
use std::path::Path;

use find_extract_types::IndexLine;
use find_extract_types::ExtractorConfig;

/// Extract version information from PE bytes.
///
/// Used by `find-extract-dispatch` for archive members. Does not include a
/// filename line — the caller adds that.
pub fn extract_from_bytes(bytes: &[u8], _name: &str, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    let version_info = extract_version_info(bytes)?;
    Ok(version_info
        .lines()
        .filter(|l| !l.is_empty())
        .map(|line| IndexLine {
            line_number: 0,
            content: line.to_string(),
            archive_path: None,
        })
        .collect())
}

/// Extract version information from PE files (EXE, DLL, etc.).
///
/// Supports:
/// - Windows executables (.exe, .dll, .sys, .scr, .cpl, .ocx)
/// - Version info resources (product, version, company, copyright, etc.)
///
/// # Returns
/// Vector of IndexLine objects with metadata at line_number=0
pub fn extract(path: &Path, _cfg: &ExtractorConfig) -> anyhow::Result<Vec<IndexLine>> {
    // Read the file
    let data = fs::read(path)?;

    // Parse as PE file
    let version_info = extract_version_info(&data)?;

    // Always include the filename in the content so the file is searchable by name
    let filename = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    // Build the full metadata content
    let full_content = if version_info.is_empty() {
        // No metadata, just the filename
        filename.to_string()
    } else {
        // Prepend filename to metadata
        format!("{}\n{}", filename, version_info)
    };

    // Split into multiple IndexLine objects (one per line) all at line_number=0
    // This ensures all metadata lines are displayed in the file viewer
    let lines: Vec<IndexLine> = full_content
        .lines()
        .map(|line| IndexLine {
            line_number: 0,
            content: line.to_string(),
            archive_path: None,
        })
        .collect();

    Ok(lines)
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
            "FileVersion: {}.{}.{}.{}",
            file_ver.Major, file_ver.Minor, file_ver.Patch, file_ver.Build
        ));

        lines.push(format!(
            "ProductVersion: {}.{}.{}.{}",
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
                lines.push(format!("{}: {}", key, value.trim()));
            }
        });
    }

    lines.join("\n")
}
