use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use colored::Colorize;

use find_common::api::{RecentAction, WorkerQueueSlot, WorkerStatus};
use find_common::config::{default_config_path, parse_client_config};

mod api;

#[derive(Parser)]
#[command(name = "find-admin", about = "Administrative utilities for find-anything", version)]
struct Args {
    /// Path to client config file (default: /etc/find-anything/client.toml as root, else ~/.config/find-anything/client.toml)
    #[arg(long, global = true)]
    config: Option<String>,
    /// Output raw JSON instead of human-readable text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print effective client configuration with defaults filled in
    Config,
    /// Print per-source statistics from the server
    Status {
        /// Refresh statistics every 2 seconds until Ctrl+C
        #[arg(long, short)]
        watch: bool,
        /// Force a full stats rebuild on the server before displaying
        #[arg(long)]
        refresh: bool,
    },
    /// List indexed sources
    Sources,
    /// Check server connectivity and authentication
    Check,
    /// Show inbox status (pending and failed files)
    Inbox,
    /// Delete inbox files
    InboxClear {
        /// Target the failed/ queue instead of pending
        #[arg(long)]
        failed: bool,
        /// Target both pending and failed queues
        #[arg(long)]
        all: bool,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Move failed inbox files back to pending for retry
    InboxRetry {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Pause inbox processing (current in-flight jobs are returned to the inbox)
    InboxPause,
    /// Resume inbox processing after a pause
    InboxResume,
    /// Remove orphaned chunks from ZIP archives to reclaim disk space
    Compact {
        /// Report what would be freed without modifying any files
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the contents of a named inbox item (searches pending and failed queues)
    InboxShow {
        /// Inbox filename, with or without .gz extension
        name: String,
    },
    /// Show recently indexed or recently modified files
    Recent {
        /// Number of files to show (default: 20)
        #[arg(long, short, default_value = "20")]
        limit: usize,
        /// Sort by file modification time (mtime) instead of index time
        #[arg(long)]
        mtime: bool,
        /// Stay connected and print new entries as they arrive (server-sent events)
        #[arg(long, short = 'f')]
        follow: bool,
    },
    /// Delete all indexed data for a source (DB + content chunks in ZIP archives)
    DeleteSource {
        /// Name of the source to delete
        source: String,
        /// Skip confirmation prompt
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();

    let args = Args::from_arg_matches(&Args::command().version(find_common::tool_version!()).get_matches()).unwrap_or_else(|e| e.exit());

    let config_path = args.config.clone().unwrap_or_else(default_config_path);
    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config: {config_path}"))?;
    let (config, config_warnings) = parse_client_config(&config_str)?;
    for w in &config_warnings { eprintln!("Warning: {w}"); }

    // Check version compatibility for all commands that talk to the server.
    if !matches!(args.command, Command::Config) {
        let client = api::ApiClient::new(&config.server.url, &config.server.token);
        client.check_server_version().await?;
    }

    match args.command {
        Command::Config => {
            if args.json {
                let json = serde_json::to_string_pretty(&config)
                    .context("serializing config to JSON")?;
                println!("{json}");
            } else {
                let toml = toml::to_string_pretty(&config)
                    .context("serializing config to TOML")?;
                println!("# Effective configuration (file: {config_path})");
                println!("# Values shown include defaults for any fields not set in your file.");
                println!();
                print!("{toml}");
            }
        }

        Command::Status { watch, refresh } => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            if args.json || !watch {
                let stats = client.get_stats(refresh).await.context("fetching stats")?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&stats)?);
                } else {
                    print!("{}", format_status(&stats));
                }
            } else {
                // Watch mode: event-driven via SSE stream — redraws on each cache update.
                use std::io::Write;
                let stream = client.stream_stats(|event| {
                    let output = format_stream_status(&event);
                    print!("\x1b[H\x1b[J{output}");
                    std::io::stdout().flush().ok();
                });
                tokio::select! {
                    result = stream => {
                        if let Err(e) = result { eprintln!("Stream error: {e:#}"); }
                    }
                    _ = tokio::signal::ctrl_c() => { println!(); }
                }
            }
        }

        Command::Sources => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            let sources = client.get_sources().await.context("fetching sources")?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&sources)?);
            } else if sources.is_empty() {
                println!("No sources indexed.");
            } else {
                for (i, s) in sources.iter().enumerate() {
                    println!("  {}. {}", i + 1, s.name);
                }
            }
        }

        Command::Check => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            let mut all_ok = true;

            // Check server reachable + authenticated via /api/v1/settings
            match client.get_settings().await {
                Ok(settings) => {
                    println!("{}", format!("✓  Server reachable at {}", config.server.url).green());
                    println!("{}", "✓  Authenticated (token accepted)".green());
                    println!("{}", format!("✓  Server version: {} (build {}, schema v{}, min client v{})", settings.version, settings.git_hash, settings.schema_version, settings.min_client_version).green());
                }
                Err(e) => {
                    // Distinguish auth failures from connectivity failures
                    let msg = e.to_string();
                    if msg.contains("401") || msg.contains("UNAUTHORIZED") || msg.contains("Unauthorized") {
                        println!("{}", format!("✓  Server reachable at {}", config.server.url).green());
                        println!("{}", "✗  Authentication failed (check token)".red());
                    } else {
                        println!("{}", format!("✗  Server not reachable at {} — {e:#}", config.server.url).red());
                        println!("{}", "✗  Authentication not checked (server unreachable)".red());
                    }
                    println!("{}", "✗  Server version: unknown".red());
                    all_ok = false;
                }
            }

            // Check sources
            match client.get_sources().await {
                Ok(sources) => {
                    println!("{}", format!("✓  {} source(s) indexed", sources.len()).green());
                }
                Err(e) => {
                    println!("{}", format!("✗  Could not fetch sources: {e:#}").red());
                    all_ok = false;
                }
            }

            if !all_ok {
                std::process::exit(1);
            }
        }

        Command::Inbox => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            let status = client.inbox_status().await.context("fetching inbox status")?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&status)?);
            } else {
                if status.paused {
                    println!("{}", "Inbox processing is PAUSED  (use `find-admin inbox-resume` to resume)".yellow());
                    println!();
                }
                println!("Pending ({}):", status.pending.len());
                for item in &status.pending {
                    println!(
                        "  {}  {}  age: {}",
                        item.filename,
                        format_bytes(item.size_bytes),
                        format_age(item.age_secs),
                    );
                }
                println!();
                println!("Archive queue ({}): requests indexed, awaiting ZIP content write", status.archive_queue);
                println!();
                println!("Failed ({}):", status.failed.len());
                for item in &status.failed {
                    println!(
                        "  {}  {}  age: {}",
                        item.filename,
                        format_bytes(item.size_bytes),
                        format_age(item.age_secs),
                    );
                }
            }
        }

        Command::InboxClear { failed, all, yes } => {
            let target = if all { "all" } else if failed { "failed" } else { "pending" };
            let client = api::ApiClient::new(&config.server.url, &config.server.token);

            if !yes {
                let status = client.inbox_status().await.context("fetching inbox status")?;
                let count = match target {
                    "all" => status.pending.len() + status.failed.len(),
                    "failed" => status.failed.len(),
                    _ => status.pending.len(),
                };
                let qualifier = if target == "all" { String::new() } else { format!("{target} ") };
                eprint!("Clear {} {}file(s)? [y/N] ", count, qualifier);
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).context("reading confirmation")?;
                match input.trim() {
                    "y" | "Y" => {}
                    _ => {
                        eprintln!("Aborted.");
                        return Ok(());
                    }
                }
            }

            let resp = client.inbox_clear(target).await.context("clearing inbox")?;
            println!("Deleted {} file(s).", resp.deleted);
        }

        Command::InboxRetry { yes } => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);

            if !yes {
                let status = client.inbox_status().await.context("fetching inbox status")?;
                eprint!("Retry {} failed file(s)? [y/N] ", status.failed.len());
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).context("reading confirmation")?;
                match input.trim() {
                    "y" | "Y" => {}
                    _ => {
                        eprintln!("Aborted.");
                        return Ok(());
                    }
                }
            }

            let resp = client.inbox_retry().await.context("retrying inbox")?;
            println!("Retried {} file(s).", resp.retried);
        }

        Command::InboxPause => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            let resp = client.inbox_pause().await.context("pausing inbox")?;
            if resp.returned > 0 {
                println!("Inbox paused. {} in-flight job(s) returned to the inbox.", resp.returned);
            } else {
                println!("Inbox paused.");
            }
        }

        Command::InboxResume => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            client.inbox_resume().await.context("resuming inbox")?;
            println!("Inbox resumed.");
        }

        Command::Compact { dry_run } => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            if dry_run {
                println!("Scanning archives (dry run — no files will be modified)...");
            } else {
                println!("Compacting archives...");
            }
            let resp = client.compact(dry_run).await.context("running compact")?;
            if resp.chunks_removed == 0 {
                println!("No orphaned chunks found across {} archive(s).", resp.archives_scanned);
            } else if dry_run {
                println!(
                    "Would free {} across {} orphaned chunk(s) in {} archive(s)  (of {} scanned).",
                    format_bytes(resp.bytes_freed),
                    resp.chunks_removed,
                    resp.archives_rewritten, // archives_rewritten == archives that would be rewritten
                    resp.archives_scanned,
                );
                println!("Run without --dry-run to apply.");
            } else {
                println!(
                    "Freed {} — rewrote {} archive(s), removed {} orphaned chunk(s).",
                    format_bytes(resp.bytes_freed),
                    resp.archives_rewritten,
                    resp.chunks_removed,
                );
            }
        }

        Command::DeleteSource { source, force } => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);

            if !force {
                let sources = client.get_sources().await.context("fetching sources")?;
                if !sources.iter().any(|s| s.name == source) {
                    eprintln!("Source '{}' not found.", source);
                    std::process::exit(1);
                }
                let stats = client.get_stats(false).await.context("fetching stats")?;
                let file_count = stats.sources.iter()
                    .find(|s| s.name == source)
                    .map(|s| s.total_files)
                    .unwrap_or(0);
                eprint!(
                    "Delete source '{}' ({} files)? This cannot be undone. [y/N] ",
                    source, file_count
                );
                let mut input = String::new();
                std::io::stdin().read_line(&mut input).context("reading confirmation")?;
                match input.trim() {
                    "y" | "Y" => {}
                    _ => {
                        eprintln!("Aborted.");
                        return Ok(());
                    }
                }
            }

            let resp = client.delete_source(&source).await.context("deleting source")?;
            println!(
                "Deleted source '{}': {} files, {} chunks removed.",
                source, resp.files_deleted, resp.chunks_removed,
            );
        }

        Command::InboxShow { name } => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            let resp = client.inbox_show(&name).await.context("fetching inbox item")?;

            let Some(resp) = resp else {
                eprintln!("Not found: {name}");
                std::process::exit(1);
            };

            if args.json {
                println!("{}", serde_json::to_string_pretty(&resp)?);
                return Ok(());
            }

            let queue_label = if resp.queue == WorkerQueueSlot::Failed {
                format!(" [{}]", "FAILED".red())
            } else {
                String::new()
            };
            println!("source:  {}{queue_label}", resp.source);
            if let Some(ts) = resp.scan_timestamp {
                let dt = chrono::DateTime::from_timestamp(ts, 0)
                    .map(|utc| chrono::DateTime::<chrono::Local>::from(utc).to_rfc2822())
                    .unwrap_or_else(|| ts.to_string());
                println!("scan_ts: {dt}");
            }
            println!();

            if !resp.files.is_empty() {
                println!("Upserts ({}):", resp.files.len());
                for f in &resp.files {
                    println!("  [{:7}]  {}  ({} content lines)", f.kind, f.path, f.content_lines);
                }
            }

            if !resp.delete_paths.is_empty() {
                println!();
                println!("Deletes ({}):", resp.delete_paths.len());
                for p in &resp.delete_paths {
                    println!("  {p}");
                }
            }

            if !resp.failures.is_empty() {
                println!();
                println!("Failures ({}):", resp.failures.len());
                for f in &resp.failures {
                    println!("  {}  —  {}", f.path, f.error);
                }
            }
        }

        Command::Recent { limit, mtime, follow } => {
            let client = api::ApiClient::new(&config.server.url, &config.server.token);
            if follow {
                // SSE follow mode: stream live events, print as they arrive.
                // The initial burst (last `limit` entries) is sent by the server
                // before the live stream begins, matching `tail -f` semantics.
                eprintln!("Streaming activity (Ctrl+C to stop)…");
                let stream = client.stream_recent(limit, mtime, |f| {
                    print_recent_line(&f);
                });
                tokio::select! {
                    result = stream => {
                        if let Err(e) = result { eprintln!("Stream error: {e:#}"); }
                    }
                    _ = tokio::signal::ctrl_c() => { eprintln!(); }
                }
            } else {
                let files = client.get_recent(limit, mtime).await.context("fetching recent files")?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&files)?);
                } else if files.is_empty() {
                    println!("No recent activity.");
                } else {
                    let label = if mtime { "modified" } else { "activity" };
                    println!("Recent {label} ({} files):", files.len());
                    for f in &files {
                        print_recent_line(f);
                    }
                }
            }
        }
    }

    Ok(())
}

fn print_recent_line(f: &find_common::api::RecentFile) {
    let ts = chrono::DateTime::from_timestamp(f.indexed_at, 0)
        .map(|utc| chrono::DateTime::<chrono::Local>::from(utc)
            .format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| f.indexed_at.to_string());
    let action_label = match f.action {
        RecentAction::Added    => "added   ",
        RecentAction::Modified => "modified",
        RecentAction::Deleted  => "deleted ",
        RecentAction::Renamed  => "renamed ",
    };
    if let Some(new_path) = &f.new_path {
        println!("  {}  [{}]  {}  {}  →  {}", ts, f.source, action_label, f.path, new_path);
    } else {
        println!("  {}  [{}]  {}  {}", ts, f.source, action_label, f.path);
    }
}

fn format_status(stats: &find_common::api::StatsResponse) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    writeln!(out, "Sources:").unwrap();
    for s in &stats.sources {
        let age = s.last_scan.map(|ts| {
            let secs = chrono_age_secs(ts);
            format_age(secs)
        }).unwrap_or_else(|| "never".to_string());
        let pending_note = if s.files_pending_content > 0 {
            format!(" ({} pending content)", s.files_pending_content)
        } else {
            String::new()
        };
        writeln!(
            out,
            "  {:20}  {:>6} files{}  {:>10}  last scan: {}",
            s.name,
            s.total_files,
            pending_note,
            format_bytes(s.total_size as u64),
            age,
        ).unwrap();
    }
    writeln!(out).unwrap();
    if stats.inbox_paused {
        writeln!(out, "Inbox:    {} pending, {} failed, {} awaiting archive  {}",
            stats.inbox_pending, stats.failed_requests, stats.archive_queue,
            "PAUSED".yellow()).unwrap();
    } else {
        writeln!(out, "Inbox:    {} pending, {} failed, {} awaiting archive",
            stats.inbox_pending, stats.failed_requests, stats.archive_queue).unwrap();
    }
    writeln!(out, "Archives: {} ZIP files ({})", stats.total_archives, format_bytes(stats.archive_size_bytes)).unwrap();
    writeln!(out, "DB size:  {}", format_bytes(stats.db_size_bytes)).unwrap();
    match (stats.orphaned_bytes, stats.orphaned_stats_age_secs) {
        (Some(orphaned), Some(age)) => {
            let pct = if stats.archive_size_bytes > 0 {
                orphaned as f64 / stats.archive_size_bytes as f64 * 100.0
            } else { 0.0 };
            writeln!(
                out,
                "Wasted:   {} ({:.1}%)  [stats {}]",
                format_bytes(orphaned), pct, format_age(age),
            ).unwrap();
        }
        _ => writeln!(out, "Wasted:   (pending first scan)").unwrap(),
    }
    match &stats.worker_status {
        WorkerStatus::Idle => writeln!(out, "Worker:   idle").unwrap(),
        WorkerStatus::Processing { source, file } =>
            writeln!(out, "Worker:   {} processing {}/{}", "●".cyan(), source, file).unwrap(),
    }
    out
}

fn format_stream_status(event: &find_common::api::StatsStreamEvent) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    writeln!(out, "Sources (live):").unwrap();
    for s in &event.sources {
        let pending_note = if s.files_pending_content > 0 {
            format!(" ({} pending content)", s.files_pending_content)
        } else {
            String::new()
        };
        writeln!(
            out,
            "  {:20}  {:>6} files{}  {:>10}",
            s.name,
            s.total_files,
            pending_note,
            format_bytes(s.total_size as u64),
        ).unwrap();
    }
    writeln!(out).unwrap();
    if event.inbox_paused {
        writeln!(out, "Inbox:    {} pending, {} failed, {} awaiting archive  {}",
            event.inbox_pending, event.failed_requests, event.archive_queue,
            "PAUSED".yellow()).unwrap();
    } else {
        writeln!(out, "Inbox:    {} pending, {} failed, {} awaiting archive",
            event.inbox_pending, event.failed_requests, event.archive_queue).unwrap();
    }
    writeln!(out, "Archives: {} ZIP files ({})", event.total_archives, format_bytes(event.archive_size_bytes)).unwrap();
    writeln!(out, "DB size:  {}", format_bytes(event.db_size_bytes)).unwrap();
    match (event.orphaned_bytes, event.orphaned_stats_age_secs) {
        (Some(orphaned), Some(age)) => {
            let pct = if event.archive_size_bytes > 0 {
                orphaned as f64 / event.archive_size_bytes as f64 * 100.0
            } else { 0.0 };
            writeln!(
                out,
                "Wasted:   {} ({:.1}%)  [stats {}]",
                format_bytes(orphaned), pct, format_age(age),
            ).unwrap();
        }
        _ => writeln!(out, "Wasted:   (pending first scan)").unwrap(),
    }
    match &event.worker_status {
        WorkerStatus::Idle => writeln!(out, "Worker:   idle").unwrap(),
        WorkerStatus::Processing { source, file } =>
            writeln!(out, "Worker:   {} processing {}/{}", "●".cyan(), source, file).unwrap(),
    }
    out
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

fn chrono_age_secs(unix_ts: i64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    (now - unix_ts).max(0) as u64
}
