mod api;
mod batch;
mod path_util;
mod subprocess;
mod upload;
mod walk;
mod watch;

use anyhow::{Context, Result};
use clap::{CommandFactory, FromArgMatches, Parser};
#[cfg(windows)]
use clap::Subcommand;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, Layer};

use find_common::config::{default_config_path, parse_client_config};
use find_common::logging::LogIgnoreFilter;
#[cfg(windows)]
use std::sync::OnceLock;

/// Config path captured in main() and shared with service_entry via OnceLock,
/// because ServiceMain args are separate from the binary's command-line args.
#[cfg(windows)]
static SERVICE_CONFIG_PATH: OnceLock<std::path::PathBuf> = OnceLock::new();

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
        // Prefer the path captured in main() via OnceLock (set before
        // service_dispatcher::start() is called).  Fall back to parsing from
        // the ServiceMain args for extra args supplied via `sc start … <args>`.
        let config_path = SERVICE_CONFIG_PATH
            .get()
            .cloned()
            .or_else(|| parse_config_from_args(&args));
        let config = match config_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| parse_client_config(&s).map(|(c, _)| c).ok())
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
        // scan_now is always false for the service — no immediate startup scan.
        let svc_config_path = config_path
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let svc_opts = watch::WatchOptions { config_path: svc_config_path, scan_now: false };
        tokio::select! {
            _ = watch::run_watch(&config, &svc_opts) => {}
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
    #[arg(long, global = true)]
    config: Option<String>,

    /// Run find-scan immediately at startup (in addition to the scheduled interval).
    #[arg(long, short = 'S')]
    scan_now: bool,

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
    let args = Args::from_arg_matches(&Args::command().version(find_common::tool_version!()).get_matches()).unwrap_or_else(|e| e.exit());
    let config_path = resolve_config(args.config);

    // On Windows, Install/Uninstall commands don't need config or logging —
    // handle them before the config read.
    #[cfg(windows)]
    if let Some(cmd @ (WindowsCommand::Install { .. } | WindowsCommand::Uninstall { .. })) = args.command {
        return run_windows_command(cmd, &config_path);
    }

    // Read config before logging init so [log] compact = true takes effect.
    // Config errors go to stderr via `?`; no logging needed for that.
    let config_str = std::fs::read_to_string(&config_path)
        .with_context(|| format!("reading config {config_path}"))?;
    let (config, config_warnings) = parse_client_config(&config_str)?;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "warn,find_watch=info".into());
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

    for w in &config_warnings { eprintln!("Warning: {w}"); }

    if let Err(e) = find_common::logging::set_ignore_patterns(&config.log.ignore) {
        tracing::warn!("invalid log ignore pattern: {e}");
    }

    // On Windows, ServiceRun dispatches to the SCM (logging is now ready).
    #[cfg(windows)]
    if let Some(cmd) = args.command {
        return run_windows_command(cmd, &config_path);
    }

    let client = api::ApiClient::new(&config.server.url, &config.server.token);
    client.check_server_version().await?;

    let opts = watch::WatchOptions {
        config_path: config_path.clone(),
        scan_now: args.scan_now,
    };
    watch::run_watch(&config, &opts).await
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
            // Store the config path so service_entry can access it.
            // (ServiceMain args are separate from the binary command-line args,
            // so we can't rely on parse_config_from_args inside service_entry.)
            let _ = SERVICE_CONFIG_PATH.set(std::path::PathBuf::from(config_path));
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
