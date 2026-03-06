mod api;

use anyhow::{Context, Result};
use clap::Parser;
use colored::Colorize;

use find_common::config::{default_config_path, parse_client_config};

#[derive(Parser)]
#[command(name = "find", about = "Search the find-anything index", version)]
struct Args {
    /// Search pattern
    pattern: String,

    /// Matching mode
    #[arg(long, default_value = "fuzzy")]
    mode: String,

    /// Only search these sources (repeatable)
    #[arg(long = "source")]
    sources: Vec<String>,

    /// Maximum results to show
    #[arg(long, default_value = "50")]
    limit: usize,

    /// Skip first N results
    #[arg(long, default_value = "0")]
    offset: usize,

    /// Lines of context to show before and after each match (like grep -C)
    #[arg(short = 'C', long, default_value = "0")]
    context: usize,

    /// Suppress color output
    #[arg(long)]
    no_color: bool,

    /// Path to client config file (default: /etc/find-anything/client.toml as root, else ~/.config/find-anything/client.toml)
    #[arg(long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();

    if args.no_color {
        colored::control::set_override(false);
    }

    let config_path = args.config.unwrap_or_else(default_config_path);
    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config {config_path}"))?;
    let config = parse_client_config(&config_str)?;

    let client = api::ApiClient::new(&config.server.url, &config.server.token);
    client.check_server_version().await?;

    let resp = client
        .search(
            &args.pattern,
            &args.mode,
            &args.sources,
            args.limit,
            args.offset,
        )
        .await?;

    if resp.results.is_empty() {
        eprintln!("no results");
        return Ok(());
    }

    let separator = "──".repeat(30).dimmed().to_string();

    for hit in &resp.results {
        let source_tag = format!("[{}]", hit.source).cyan().to_string();
        let path_str = match &hit.archive_path {
            Some(inner) => format!("{}::{}", hit.path, inner),
            None => hit.path.clone(),
        };
        let loc = format!("{}:{}", path_str, hit.line_number).green().to_string();

        if args.context == 0 {
            let snippet = hit.snippet.trim();
            println!("{} {}  {}", source_tag, loc, snippet);
        } else {
            println!("{}", separator);
            println!("{} {}", source_tag, loc);

            let ctx = client
                .context(
                    &hit.source,
                    &hit.path,
                    hit.archive_path.as_deref(),
                    hit.line_number,
                    args.context,
                )
                .await?;

            for (i, content) in ctx.lines.iter().enumerate() {
                let line_num = ctx.start + i;
                if Some(i) == ctx.match_index {
                    // Matching line: highlighted
                    let marker = ">".yellow().bold().to_string();
                    let num = format!("{:>5}", line_num).green().to_string();
                    println!("{} {}  {}", marker, num, content);
                } else {
                    // Context line: dimmed
                    let num = format!("{:>5}", line_num).dimmed().to_string();
                    println!("  {}  {}", num, content.dimmed());
                }
            }
        }
    }

    eprintln!("({} total)", resp.total);
    Ok(())
}
