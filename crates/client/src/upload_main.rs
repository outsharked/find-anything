mod api;
mod upload;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};

use find_common::config::{default_config_path, parse_client_config};

#[derive(Parser)]
#[command(
    name = "find-upload",
    about = "Upload a file to the find-anything server for server-side indexing",
    version
)]
struct Args {
    /// Path to client config file (default: /etc/find-anything/client.toml as root, else ~/.config/find-anything/client.toml)
    #[arg(long)]
    config: Option<String>,

    /// Source name to index the file under
    #[arg(long)]
    source: String,

    /// Relative path to store in the index (defaults to the file name)
    #[arg(long)]
    rel_path: Option<String>,

    /// File to upload
    file: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,find_upload=info".into()),
        )
        .init();

    let args = Args::from_arg_matches(&Args::command().version(find_common::tool_version!()).get_matches()).unwrap_or_else(|e| e.exit());

    let config_path = args.config.unwrap_or_else(default_config_path);
    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config {config_path}"))?;
    let (config, config_warnings) = parse_client_config(&config_str)?;
    for w in &config_warnings { eprintln!("Warning: {w}"); }

    let abs_path = args.file.canonicalize().context("resolving file path")?;
    let mtime = abs_path
        .metadata()
        .context("stat file")?
        .modified()
        .context("reading mtime")?
        .duration_since(std::time::UNIX_EPOCH)
        .context("mtime before epoch")?
        .as_secs() as i64;

    let rel_path = args.rel_path.unwrap_or_else(|| {
        abs_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| abs_path.to_string_lossy().into_owned())
    });

    let client = api::ApiClient::new(&config.server.url, &config.server.token);
    client.check_server_version().await?;

    eprintln!("Uploading {} as {rel_path} into source '{}'", abs_path.display(), args.source);

    upload::upload_file(&client, &abs_path, &rel_path, mtime, &args.source)
        .await
        .context("upload failed")?;

    eprintln!("Done. The server will index the file shortly.");
    Ok(())
}
