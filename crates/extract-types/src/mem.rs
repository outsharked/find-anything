/// Read the kernel's estimate of available memory from /proc/meminfo (Linux only).
///
/// Returns `MemAvailable` in bytes, which includes free RAM plus reclaimable
/// page cache. Returns `None` on non-Linux platforms or if the file is
/// unreadable (e.g. inside a container with a restricted /proc).
#[cfg(target_os = "linux")]
pub fn available_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn available_bytes() -> Option<u64> {
    None
}
