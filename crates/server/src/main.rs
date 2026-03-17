use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};
use tracing::warn;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use find_common::config::{default_server_config_path, parse_server_config};
use find_common::logging::LogIgnoreFilter;
use find_server::{build_router, create_app_state};

#[derive(Parser)]
#[command(name = "find-server", about = "find-anything index server", version)]
struct Args {
    /// Path to server config file.
    /// Defaults to $XDG_CONFIG_HOME/find-anything/server.toml,
    /// or /etc/find-anything/server.toml when running as root.
    #[arg(long, env = "FIND_ANYTHING_SERVER_CONFIG")]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse args first so we know the config path before initialising logging.
    let args = Args::from_arg_matches(&Args::command().version(find_common::tool_version!()).get_matches()).unwrap_or_else(|e| e.exit());
    let config_path = args.config.unwrap_or_else(default_server_config_path);

    // Read config before logging init so [log] compact = true takes effect.
    // Config errors go to stderr via `?`; no logging needed for that.
    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config: {config_path}"))?;
    let (config, config_warnings) = parse_server_config(&config_str)?;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,find_server=info,tower_http=info".into());

    if config.log.compact {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer()
                .without_time()
                .with_target(false)
                .with_filter(LogIgnoreFilter))
            .init();
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer().with_filter(LogIgnoreFilter))
            .init();
    }

    for w in &config_warnings { warn!("{w}"); }

    if let Err(e) = find_common::logging::set_ignore_patterns(&config.log.ignore) {
        tracing::warn!("invalid log ignore pattern: {e}");
    }

    let bind = config.server.bind.clone();

    let state = create_app_state(config).await?;
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding to {bind}"))?;

    tracing::info!("listening on {bind}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .context("server error")?;

    Ok(())
}
