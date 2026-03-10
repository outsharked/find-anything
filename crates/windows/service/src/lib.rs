//! Windows Service support for find-watch.
//!
//! Provides `install_service` and `uninstall_service` for managing the
//! `FindAnythingWatcher` Windows Service.
//!
//! The `service_main` entry point lives in `find-watch`'s `watch_main.rs`
//! because `define_windows_service!` emits a public FFI symbol that must
//! reside in the binary crate.

#![cfg(windows)]

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use windows_service::{
    service::{
        ServiceAccess, ServiceErrorControl, ServiceInfo, ServiceStartType, ServiceState,
        ServiceType,
    },
    service_manager::{ServiceManager, ServiceManagerAccess},
};
use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
use winreg::RegKey;

pub const SERVICE_NAME: &str = "FindAnythingWatcher";
const SERVICE_DISPLAY_NAME: &str = "Find Anything Watcher";
const SERVICE_DESCRIPTION: &str =
    "Find Anything file watcher \u{2014} keeps the index current. \
     https://github.com/jamietre/find-anything";
const REGISTRY_RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const REGISTRY_VALUE_NAME: &str = "FindAnythingTray";

/// Register the Find Anything watcher as a Windows Service and add the tray
/// app to the current user's startup run key.
///
/// Requires Administrator privileges.
pub fn install_service(config_path: &Path, service_name: &str) -> Result<()> {
    let manager = ServiceManager::local_computer(
        None::<&str>,
        ServiceManagerAccess::CREATE_SERVICE,
    )
    .context("opening Service Control Manager (run as administrator)")?;

    let current_exe = std::env::current_exe().context("resolving current executable path")?;

    let config_abs = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());

    let service_info = ServiceInfo {
        name: OsString::from(service_name),
        display_name: OsString::from(SERVICE_DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: current_exe.clone(),
        launch_arguments: vec![
            OsString::from("service-run"),
            OsString::from("--config"),
            config_abs.clone().into_os_string(),
        ],
        dependencies: vec![],
        account_name: None,
        account_password: None,
    };

    // If the service already exists (e.g. reinstall/upgrade), delete it first
    // so we can recreate it with the latest configuration.
    if let Ok(existing) = manager.open_service(
        service_name,
        ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
    ) {
        let status = existing.query_status().ok();
        let is_running = status.map_or(false, |s| {
            s.current_state != ServiceState::Stopped
                && s.current_state != ServiceState::StopPending
        });
        if is_running {
            let _ = existing.stop();
            // Wait up to 15 s for the service to reach Stopped.
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            loop {
                std::thread::sleep(Duration::from_millis(200));
                let stopped = existing.query_status()
                    .map(|s| s.current_state == ServiceState::Stopped)
                    .unwrap_or(true);
                if stopped || std::time::Instant::now() > deadline { break; }
            }
        }
        let _ = existing.delete();
        // Drop our handle so we are not the reason the service lingers.
        drop(existing);
        // Poll until the SCM no longer knows about the service name (all
        // handles closed) before calling CreateService.  Without this,
        // CreateService returns ERROR_SERVICE_MARKED_FOR_DELETE if another
        // process (e.g. the tray) still holds an open handle.
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            let gone = manager
                .open_service(service_name, ServiceAccess::QUERY_STATUS)
                .is_err();
            if gone || std::time::Instant::now() > deadline { break; }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    let service = manager
        .create_service(&service_info, ServiceAccess::CHANGE_CONFIG)
        .context("creating Windows service")?;

    service
        .set_description(SERVICE_DESCRIPTION)
        .context("setting service description")?;

    // Grant BUILTIN\Users the ability to start, stop, and query the service so
    // the tray app can control it without requiring Administrator privileges.
    // SDDL breakdown:
    //   SY = Local System (full control)
    //   BA = Administrators (full control)
    //   BU = Users (start RP, stop WP, query LC/CC/SW/LO/CR, read RC)
    //   IU = Interactive Users (query only)
    //   SU = Service Users (query only)
    let sddl = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)\
                 (A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)\
                 (A;;CCLCSWRPWPDTLOCRRC;;;BU)\
                 (A;;CCLCSWLOCRRC;;;IU)\
                 (A;;CCLCSWLOCRRC;;;SU)";
    let _ = std::process::Command::new("sc.exe")
        .args(["sdset", service_name, sddl])
        .output(); // best-effort; non-fatal if it fails

    // Register tray app in HKCU Run so it starts at user login.
    let tray_exe = current_exe
        .parent()
        .map(|p| p.join("find-tray.exe"))
        .unwrap_or_else(|| std::path::PathBuf::from("find-tray.exe"));

    let run_value = format!(
        "\"{}\" --config \"{}\"",
        tray_exe.display(),
        config_abs.display()
    );

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let run_key = hkcu
        .open_subkey_with_flags(REGISTRY_RUN_KEY, KEY_SET_VALUE)
        .context("opening HKCU Run registry key")?;
    run_key
        .set_value(REGISTRY_VALUE_NAME, &run_value)
        .context("writing tray app to Run registry")?;

    println!("Service '{service_name}' installed successfully.");
    println!("Tray app registered to start at login: {run_value}");
    println!();
    println!("Start the service now with:");
    println!("  sc start {service_name}");
    println!("Or reboot for auto-start.");

    Ok(())
}

/// Stop and delete the Find Anything watcher service, and remove the tray
/// app from the current user's startup run key.
///
/// Requires Administrator privileges.
pub fn uninstall_service(service_name: &str) -> Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
        .context("opening Service Control Manager (run as administrator)")?;

    let service = manager
        .open_service(
            service_name,
            ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
        )
        .context("opening service (is it installed?)")?;

    // Stop the service if it's running.
    let status = service.query_status().context("querying service status")?;
    if status.current_state != ServiceState::Stopped
        && status.current_state != ServiceState::StopPending
    {
        service.stop().context("sending stop signal to service")?;

        // Wait up to 30 seconds for the service to stop.
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let s = service.query_status().context("querying service status")?;
            if s.current_state == ServiceState::Stopped {
                break;
            }
            if std::time::Instant::now() > deadline {
                anyhow::bail!("timed out waiting for service '{service_name}' to stop");
            }
        }
    }

    service.delete().context("deleting service")?;

    // Remove tray app from HKCU Run.
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(run_key) = hkcu.open_subkey_with_flags(REGISTRY_RUN_KEY, KEY_SET_VALUE) {
        let _ = run_key.delete_value(REGISTRY_VALUE_NAME);
    }

    println!("Service '{service_name}' uninstalled.");
    println!("Tray app startup entry removed.");

    Ok(())
}
