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
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "warn,find_scan=info".into()))
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

    let opts = ScanOptions {
        upgrade: args.upgrade,
        quiet: args.quiet,
        dry_run: args.dry_run,
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
