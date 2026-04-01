mod api;
mod batch;
mod extract;
mod lazy_header;
mod path_util;
mod scan;
mod subprocess;
mod upload;
mod walk;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
use find_common::config::{default_config_path, parse_client_config};
use find_common::logging::LogIgnoreFilter;
use scan::{ScanOptions, ScanSource};

#[derive(Parser)]
#[command(name = "find-scan", about = "Index files and submit to find-anything server", version)]
struct Args {
    /// Path to client config file (default: /etc/find-anything/client.toml as root, else ~/.config/find-anything/client.toml)
    #[arg(long)]
    config: Option<String>,

    /// Re-index files that were indexed by an older version of the scanner,
    /// even if their mtime has not changed. Naturally resumable: files already
    /// at the current scanner version are skipped on subsequent runs.
    #[arg(long)]
    upgrade: bool,

    /// Force re-index of all files regardless of mtime or scanner version.
    /// Useful after changing normalizer/formatter config.
    /// Optionally supply a timestamp to resume an interrupted run; files with
    /// indexed_at >= TIMESTAMP are skipped (already done). Accepts a Unix epoch
    /// (seconds), a date ("2026-03-20"), or a local datetime ("2026-03-20T18:46:38"
    /// or "2026-03-20 18:46:38"). Date-only values use midnight local time.
    /// If omitted, uses the current time and prints the epoch so you can resume
    /// if the run is interrupted.
    #[arg(long, value_name = "TIME", num_args = 0..=1, default_missing_value = "now")]
    force: Option<String>,

    /// Suppress per-file processing logs (only log warnings, errors, and summary)
    #[arg(long)]
    quiet: bool,

    /// Dry run: walk the filesystem and compare with the server's current state,
    /// but do not extract content or submit any changes. Prints a summary of
    /// how many files would be added, modified, unchanged, and deleted.
    /// Cannot be combined with a single-file argument.
    #[arg(long)]
    dry_run: bool,

    /// Scan a single file or directory instead of all configured sources.
    /// The path must be under one of the configured source paths.
    /// For a file: mtime checking is skipped — the file is always (re-)indexed.
    /// For a directory: all files under it are re-indexed (mtime is ignored).
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,

    /// Override the mtime stored for the indexed file (Unix seconds).
    /// Only valid with a single-file PATH argument.
    /// Used by the upload delegation path so find-scan stores the original
    /// file mtime rather than the temp file's creation time.
    #[arg(long, value_name = "SECS")]
    mtime: Option<i64>,
}

/// Parse a `--force` timestamp value into a Unix epoch (seconds).
///
/// Accepts:
/// - Unix epoch integer: `1742486798`
/// - Date only (local midnight): `2026-03-20`
/// - Local datetime (space or T separator): `2026-03-20 18:46:38` / `2026-03-20T18:46:38`
fn parse_force_timestamp(s: &str) -> anyhow::Result<i64> {
    // Try plain integer epoch first.
    if let Ok(epoch) = s.parse::<i64>() {
        return Ok(epoch);
    }

    // Try datetime with T separator, then space separator.
    let fmts_dt = ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%d %H:%M:%S"];
    for fmt in &fmts_dt {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return local_to_epoch(ndt);
        }
    }

    // Try date-only (treat as start of day in local timezone).
    if let Ok(nd) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return local_to_epoch(nd.and_hms_opt(0, 0, 0).unwrap());
    }

    anyhow::bail!("unrecognised timestamp format")
}

fn epoch_to_human(epoch: i64) -> String {
    Local.timestamp_opt(epoch, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| epoch.to_string())
}

fn local_to_epoch(ndt: NaiveDateTime) -> anyhow::Result<i64> {
    Local.from_local_datetime(&ndt)
        .single()
        .map(|dt| dt.timestamp())
        .ok_or_else(|| anyhow::anyhow!("ambiguous or invalid local time (near DST transition)"))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "warn,find_scan=info,nom_exif=off".into()))
        .with(lazy_header::FileHeaderLayer)
        .with(tracing_subscriber::fmt::layer().with_filter(LogIgnoreFilter))
        .init();

    let args = Args::from_arg_matches(&Args::command().version(find_common::tool_version!()).get_matches()).unwrap_or_else(|e| e.exit());

    let config_path = args.config.unwrap_or_else(default_config_path);
    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config {config_path}"))?;
    let (config, config_warnings) = parse_client_config(&config_str)?;
    for w in &config_warnings { eprintln!("Warning: {w}"); }

    if let Err(e) = find_common::logging::set_ignore_patterns(&config.log.ignore) {
        tracing::warn!("invalid log ignore pattern: {e}");
    }

    let client = api::ApiClient::new(&config.server.url, &config.server.token);
    client.check_server_version().await?;

    if config.sources.is_empty() {
        tracing::info!("No sources configured — nothing to scan.");
        return Ok(());
    }

    let force_since: Option<i64> = match args.force.as_deref() {
        None => None,
        Some("now") => {
            let epoch = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let human = epoch_to_human(epoch);
            eprintln!("Force re-index started at {human}.");
            eprintln!("If interrupted, resume with: find-scan --force {epoch}");
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("\nInterrupted. To resume, run: find-scan --force {epoch}");
                    std::process::exit(130);
                }
            });
            Some(epoch)
        }
        Some(s) => {
            let epoch = parse_force_timestamp(s)
                .with_context(|| format!("--force value {s:?} is not a recognised timestamp (try a Unix epoch, \"2026-03-20\", \"2026-03-20T18:46:38\", or \"2026-03-20 18:46:38\")"))?;
            let human = epoch_to_human(epoch);
            eprintln!("Resuming force re-index from {human}.");
            Some(epoch)
        }
    };

    let opts = ScanOptions {
        upgrade: args.upgrade,
        quiet: args.quiet,
        dry_run: args.dry_run,
        force_since,
        mtime_override: args.mtime,
        force_index: force_since.is_some(),
    };

    // Single-file mode: scan one specific file and exit.
    if opts.dry_run && args.path.as_ref().is_some_and(|p| p.is_file()) {
        anyhow::bail!("--dry-run cannot be combined with a single-file argument");
    }

    if let Some(path) = args.path {
        let abs = std::fs::canonicalize(&path)
            .with_context(|| format!("cannot access {}", path.display()))?;
        anyhow::ensure!(
            abs.is_file() || abs.is_dir(),
            "{} is not a file or directory", abs.display()
        );

        // Find the source whose configured path is the longest prefix of `abs`.
        let mut best: Option<(&find_common::config::SourceConfig, PathBuf, PathBuf)> = None;
        for source in &config.sources {
            let root_canon = std::fs::canonicalize(&source.path).unwrap_or_else(|_| PathBuf::from(&source.path));
            if let Ok(rel) = abs.strip_prefix(&root_canon) {
                let longer = best.as_ref()
                    .is_none_or(|(_, rc, _)| root_canon.as_os_str().len() > rc.as_os_str().len());
                if longer {
                    best = Some((source, root_canon, rel.to_path_buf()));
                }
            }
        }
        let (source, _, rel) = best.ok_or_else(|| {
            let paths = config.sources.iter()
                .map(|s| s.path.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::anyhow!(
                "{} is not under any configured source path\nConfigured paths: {paths}",
                abs.display()
            )
        })?;

        if abs.is_file() {
            let rel_path = path_util::normalise_path_sep(&rel.to_string_lossy());
            tracing::info!("Scanning single file: {} (source: {}, rel: {})", abs.display(), source.name, rel_path);
            let scan_source = ScanSource {
                name: &source.name,
                paths: std::slice::from_ref(&source.path),
                include: &source.include,
                subdir: None,
            };
            scan::scan_single_file(&client, &scan_source, &rel_path, &abs, &config.scan, &opts).await?;
        } else {
            // Directory: rescan all files under it, ignoring mtime.
            let rel_path = path_util::normalise_path_sep(&rel.to_string_lossy());
            let subdir = if rel_path.is_empty() { None } else { Some(rel_path.clone()) };
            let subdir_label = if rel_path.is_empty() { "(source root)" } else { &rel_path };
            tracing::info!(
                "Scanning directory: {} (source: {}, subdir: {})",
                abs.display(), source.name, subdir_label
            );
            let scan_source = ScanSource {
                name: &source.name,
                paths: std::slice::from_ref(&source.path),
                include: &source.include,
                subdir,
            };
            scan::run_scan(&client, &scan_source, &config.scan, &opts).await?;
        }
        return Ok(());
    }

    // Scan all configured sources
    for source in &config.sources {
        tracing::info!("Scanning source: {}", source.name);
        let scan_source = ScanSource {
            name: &source.name,
            paths: std::slice::from_ref(&source.path),
            include: &source.include,
            subdir: None,
        };
        scan::run_scan(&client, &scan_source, &config.scan, &opts).await?;
    }

    Ok(())
}
