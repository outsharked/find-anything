use std::process::Command;

fn main() {
    // Rerun when the commit or refs change.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");

    let hash = git("rev-parse", &["--short", "HEAD"]).unwrap_or_else(|| "unknown".into());

    let tag = git("tag", &["--points-at", "HEAD"])
        .map(|s| s.lines().next().unwrap_or("").trim().to_string())
        .unwrap_or_default();

    let dirty = git("status", &["--porcelain"])
        .map(|s| if s.trim().is_empty() { String::new() } else { "1".into() })
        .unwrap_or_default();

    println!("cargo:rustc-env=GIT_HASH={hash}");
    println!("cargo:rustc-env=GIT_TAG={tag}");
    println!("cargo:rustc-env=GIT_DIRTY={dirty}");
}

fn git(subcmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg(subcmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8(out.stdout).ok()).flatten()
}
