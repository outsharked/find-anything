mod api;
mod batch;
mod subprocess;
mod watch;

use anyhow::{Context, Result};
use clap::Parser;
#[cfg(windows)]
use clap::Subcommand;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use find_common::config::{default_config_path, parse_client_config};
use find_common::logging::LogIgnoreFilter;
#[cfg(windows)]
use find_common::config::ClientConfig;

// ── Windows Service boilerplate ───────────────────────────────────────────────
//
// The `define_windows_service!` macro emits a public FFI symbol and therefore
// must live in the binary crate (not in a library).  It delegates to
// `service_entry`, which sets up a tokio runtime and runs the watcher.

#[cfg(windows)]
windows_service::define_windows_service!(ffi_service_main, service_entry);

/// Stop flag set by the SCM Stop/Shutdown control event.
#[cfg(windows)]
static SERVICE_STOP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(windows)]
fn service_entry(args: Vec<std::ffi::OsString>) {
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use windows_service::{
        service::{
            ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus,
            ServiceType,
        },
        service_control_handler::{self, ServiceControlHandlerResult},
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("tokio runtime: {e}");
            return;
        }
    };

    rt.block_on(async {
        // Parse --config <path> from the launch arguments recorded in the SCM.
        let config_path = parse_config_from_args(&args);
        let config = match config_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| toml::from_str::<ClientConfig>(&s).ok())
        {
            Some(c) => c,
            None => {
                tracing::error!("service: failed to load config from {:?}", config_path);
                return;
            }
        };

        let event_handler = move |ctrl| match ctrl {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                SERVICE_STOP.store(true, Ordering::Relaxed);
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        };

        let status_handle = match service_control_handler::register(
            find_windows_service::SERVICE_NAME,
            event_handler,
        ) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!("register service handler: {e}");
                return;
            }
        };

        let _ = status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Running,
            controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });

        // Run the watcher until the SCM sends Stop.
        tokio::select! {
            _ = watch::run_watch(&config) => {}
            _ = async {
                loop {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    if SERVICE_STOP.load(Ordering::Relaxed) { break; }
                }
            } => {}
        }

        let _ = status_handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: ServiceState::Stopped,
            controls_accepted: ServiceControlAccept::empty(),
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::default(),
            process_id: None,
        });
    });
}

#[cfg(windows)]
fn parse_config_from_args(args: &[std::ffi::OsString]) -> Option<std::path::PathBuf> {
    let strings: Vec<String> = args
        .iter()
        .filter_map(|a| a.to_str().map(str::to_string))
        .collect();
    let idx = strings.iter().position(|s| s == "--config")?;
    strings.get(idx + 1).map(std::path::PathBuf::from)
}

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "find-watch", about = "Watch filesystem and update index in real-time", version)]
struct Args {
    /// Path to the client config file.
    #[arg(long)]
    config: Option<String>,

    #[cfg(windows)]
    #[command(subcommand)]
    command: Option<WindowsCommand>,
}

fn resolve_config(config: Option<String>) -> String {
    config.unwrap_or_else(|| {
        if cfg!(windows) {
            "client.toml".to_string()
        } else {
            default_config_path()
        }
    })
}

/// Windows-only subcommands for service management.
#[cfg(windows)]
#[derive(Subcommand)]
enum WindowsCommand {
    /// Install and start find-watch as a Windows Service (requires admin).
    Install {
        /// Windows service name.
        #[arg(long, default_value = find_windows_service::SERVICE_NAME)]
        service_name: String,
    },
    /// Uninstall the find-watch Windows Service (requires admin).
    Uninstall {
        /// Windows service name.
        #[arg(long, default_value = find_windows_service::SERVICE_NAME)]
        service_name: String,
    },
    /// Called by the Windows Service Control Manager only.
    #[command(hide = true)]
    ServiceRun,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "warn,find_watch=info".into()))
        .with(tracing_subscriber::fmt::layer().with_filter(LogIgnoreFilter))
        .init();

    let args = Args::parse();
    let config_path = resolve_config(args.config);

    #[cfg(windows)]
    if let Some(cmd) = args.command {
        return run_windows_command(cmd, &config_path);
    }

    // Default: run the watcher in the foreground.
    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config {config_path}"))?;
    let config = parse_client_config(&config_str)?;

    if let Err(e) = find_common::logging::set_ignore_patterns(&config.log.ignore) {
        tracing::warn!("invalid log ignore pattern: {e}");
    }

    let client = api::ApiClient::new(&config.server.url, &config.server.token);
    client.check_server_version().await?;

    watch::run_watch(&config).await
}

#[cfg(windows)]
fn run_windows_command(cmd: WindowsCommand, config_path: &str) -> Result<()> {
    match cmd {
        WindowsCommand::Install { service_name } => {
            find_windows_service::install_service(
                std::path::Path::new(config_path),
                &service_name,
            )
        }
        WindowsCommand::Uninstall { service_name } => {
            find_windows_service::uninstall_service(&service_name)
        }
        WindowsCommand::ServiceRun => {
            // Hand control to the SCM dispatcher; it will call ffi_service_main.
            windows_service::service_dispatcher::start(
                find_windows_service::SERVICE_NAME,
                ffi_service_main,
            )
            .context("starting service dispatcher")?;
            Ok(())
        }
    }
}
