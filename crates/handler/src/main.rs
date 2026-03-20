#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
//! `find-handler` — custom protocol handler for the `findanything://` URL scheme.
//!
//! Registered as the handler for `findanything://` URLs by the installer.
//! The browser passes the full URL as the first command-line argument:
//!   `findanything://open?path=C%3A%5CShare%5Cdocs%5Creport.pdf`
//!
//! The binary URL-decodes the `path` parameter and opens the file's location
//! in the OS file manager, then exits immediately.

fn main() {
    let url_str = match std::env::args().nth(1) {
        Some(s) => s,
        None => {
            eprintln!("find-handler: expected URL as first argument");
            std::process::exit(1);
        }
    };

    let url = match url::Url::parse(&url_str) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("find-handler: invalid URL '{url_str}': {e}");
            std::process::exit(1);
        }
    };

    if url.scheme() != "findanything" {
        eprintln!("find-handler: unexpected scheme '{}'", url.scheme());
        std::process::exit(1);
    }

    let path = match url
        .query_pairs()
        .find(|(k, _)| k == "path")
        .map(|(_, v)| v.into_owned())
    {
        Some(p) => p,
        None => {
            eprintln!("find-handler: missing 'path' query parameter");
            std::process::exit(1);
        }
    };

    open_in_file_manager(&path);
}

#[cfg(target_os = "windows")]
fn open_in_file_manager(path: &str) {
    use std::os::windows::process::CommandExt;
    // `CREATE_NO_WINDOW` (0x08000000) prevents a console flash.
    // Explorer parses its own command line raw, so we use raw_arg.
    //
    // `/select,"path"` works reliably for local drive-letter paths but is
    // unreliable for UNC / virtual paths (\\wsl.localhost\..., \\server\share).
    // For those, open the parent directory instead — Explorer handles it fine.
    let safe = path.replace('"', "");
    let is_local_drive = safe.len() >= 2 && safe.as_bytes()[1] == b':';
    let is_dir = std::path::Path::new(path).is_dir();

    let raw = if is_local_drive && !is_dir {
        format!("/select,\"{}\"", safe)
    } else {
        // For directories, UNC paths, or anything else: open the containing folder.
        let folder = if is_dir {
            safe.clone()
        } else {
            std::path::Path::new(&safe)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(&safe)
                .to_string()
        };
        format!("\"{}\"", folder)
    };

    let _ = std::process::Command::new("explorer.exe")
        .raw_arg(&raw)
        .creation_flags(0x08000000)
        .spawn();
}

#[cfg(target_os = "macos")]
fn open_in_file_manager(path: &str) {
    // `open -R` reveals the item in Finder.
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn open_in_file_manager(path: &str) {
    // Linux: xdg-open the parent directory (revealing a specific file is not
    // universally supported across file managers).
    let parent = std::path::Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or(path);
    let _ = std::process::Command::new("xdg-open").arg(parent).spawn();
}
