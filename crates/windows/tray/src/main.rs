//! find-tray: Windows system tray app for find-anything.
//!
//! Starts at login (registered by `find-watch install`), shows service status,
//! file counts, and provides quick actions for scan / start / stop.
//! Left-clicking the tray icon shows a borderless popup listing recently
//! indexed files; right-clicking shows the context menu.

// Suppress the console window on Windows.
#![cfg_attr(windows, windows_subsystem = "windows")]

// On non-Windows this binary is a stub.
#[cfg(not(windows))]
fn main() {
    eprintln!("find-tray is only supported on Windows.");
    std::process::exit(1);
}

#[cfg(windows)]
mod menu;
#[cfg(windows)]
mod poller;
#[cfg(windows)]
mod popup;
#[cfg(windows)]
mod service_ctl;

#[cfg(windows)]
use std::path::PathBuf;
#[cfg(windows)]
use std::sync::mpsc;

#[cfg(windows)]
use anyhow::{Context, Result};
#[cfg(windows)]
use find_common::{api::RecentFile, config::ClientConfig};
#[cfg(windows)]
use tray_icon::{
    menu::MenuEvent,
    MouseButton, MouseButtonState,
    TrayIcon, TrayIconBuilder, TrayIconEvent,
};

// NOTE: Stable notification-area GUID (NIF_GUID / guidItem in NOTIFYICONDATA) would
// prevent users needing to re-pin the tray icon after each update, but tray-icon 0.21
// does not expose this API.  A future upgrade or manual Shell_NotifyIconW call would
// be needed to implement it.
#[cfg(windows)]
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
};

/// Show a modal error dialog.  Safe to call before the event loop starts.
#[cfg(windows)]
fn show_error(title: &str, message: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let title_w: Vec<u16> = OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let msg_w: Vec<u16> = OsStr::new(message)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    unsafe {
        windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW(
            0,
            msg_w.as_ptr(),
            title_w.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_OK
                | windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }
}

/// Events sent from the poller thread to the main thread.
#[cfg(windows)]
#[derive(Debug)]
pub enum AppEvent {
    StatusUpdate {
        service_running: bool,
        file_count: Option<u64>,
        source_count: Option<usize>,
        recent_files: Vec<RecentFile>,
    },
}

#[cfg(windows)]
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "find_tray=info".into()),
        )
        .init();

    let config_path = parse_config_arg();
    let config_str = match std::fs::read_to_string(&config_path) {
        Ok(s) => s,
        Err(e) => {
            show_error(
                "Find Anything — Config Error",
                &format!(
                    "Could not read config file:\n{}\n\n{e}\n\nPlease run the installer or create the config manually.",
                    config_path.display()
                ),
            );
            return Err(e).with_context(|| format!("reading config {}", config_path.display()));
        }
    };
    let config: ClientConfig = match toml::from_str(&config_str) {
        Ok(c) => c,
        Err(e) => {
            show_error(
                "Find Anything — Config Error",
                &format!(
                    "Could not parse config file:\n{}\n\n{e}\n\nPlease fix the TOML syntax or re-run the installer.",
                    config_path.display()
                ),
            );
            return Err(e).context("parsing client config");
        }
    };

    let server_url = config.server.url.trim_end_matches('/').to_string();
    let token = config.server.token.clone();
    let poll_interval_ms = config.tray.poll_interval_ms;

    // Register the popup window class and create the (hidden) popup window
    // eagerly so we have a valid HWND for the right-click context menu.
    popup::register_class().context("registering popup window class")?;
    let popup = popup::Popup::create().context("creating popup window")?;

    // Build event loop with user-event type for cross-thread messaging.
    let event_loop = EventLoop::<AppEvent>::with_user_event()
        .build()
        .context("creating event loop")?;

    let proxy = event_loop.create_proxy();

    // Spawn background poller; it sends AppEvent via the mpsc channel.
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let poller = poller::spawn(tx, server_url, token, poll_interval_ms);

    // Bridge the mpsc channel to the winit proxy in a helper thread.
    std::thread::spawn(move || {
        while let Ok(event) = rx.recv() {
            if proxy.send_event(event).is_err() {
                break;
            }
        }
    });

    let tray_menu = menu::TrayMenu::new().context("building tray menu")?;

    let active_icon = load_icon(include_bytes!("../assets/icon_active.ico"))
        .context("loading active icon")?;
    let stopped_icon = load_icon(include_bytes!("../assets/icon_stopped.ico"))
        .context("loading stopped icon")?;

    // Do NOT attach the menu via with_menu(): tray-icon shows it on both
    // left and right click when attached.  We show it manually on right-click.
    let tray_icon = TrayIconBuilder::new()
        .with_tooltip("Find Anything")
        .with_icon(active_icon.clone())
        .build()
        .context("building tray icon")?;

    let mut app = TrayApp {
        tray_icon,
        tray_menu,
        active_icon,
        stopped_icon,
        config_path,
        service_running: false,
        should_quit: false,
        poller,
        popup,
        last_recent_files: vec![],
    };

    event_loop
        .run_app(&mut app)
        .context("running event loop")?;

    Ok(())
}

#[cfg(windows)]
struct TrayApp {
    tray_icon: TrayIcon,
    tray_menu: menu::TrayMenu,
    active_icon: tray_icon::Icon,
    stopped_icon: tray_icon::Icon,
    config_path: PathBuf,
    service_running: bool,
    should_quit: bool,
    poller: poller::PollerHandle,
    popup: popup::Popup,
    last_recent_files: Vec<RecentFile>,
}

#[cfg(windows)]
impl ApplicationHandler<AppEvent> for TrayApp {
    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        // No windows to create; the tray icon is already set up.
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
        // No windows owned by this app.
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::StatusUpdate {
                service_running,
                file_count,
                source_count,
                recent_files,
            } => {
                self.service_running = service_running;
                self.tray_menu
                    .update_status(service_running, file_count, source_count);

                // Update the popup list if it is currently visible.
                self.last_recent_files = recent_files;
                if self.popup.is_visible() {
                    self.popup.update_files(&self.last_recent_files);
                }

                // Swap tray icon based on service state.
                let icon = if service_running {
                    self.active_icon.clone()
                } else {
                    self.stopped_icon.clone()
                };
                let _ = self.tray_icon.set_icon(Some(icon));

                // Update tooltip.
                let tooltip = if service_running {
                    "Find Anything \u{2014} Watcher Running"
                } else {
                    "Find Anything \u{2014} Watcher Stopped"
                };
                let _ = self.tray_icon.set_tooltip(Some(tooltip));
            }
        }

        if self.should_quit {
            event_loop.exit();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Capture the close request BEFORE processing tray click events so that
        // a left-click that dismissed the popup (via WM_ACTIVATE/WA_INACTIVE)
        // does not immediately reopen it.
        let close_was_requested = popup::take_close_request();
        if close_was_requested {
            self.popup.hide();
            self.poller.set_active(false);
        }

        // Poll tray icon events (clicks).
        while let Ok(tray_event) = TrayIconEvent::receiver().try_recv() {
            match tray_event {
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    // Suppress toggle if this click is the one that caused the
                    // WM_ACTIVATE/WA_INACTIVE dismissal above.
                    if !close_was_requested {
                        self.toggle_popup();
                    }
                }
                TrayIconEvent::Click {
                    button: MouseButton::Right,
                    button_state: MouseButtonState::Up,
                    ..
                } => {
                    // Show the context menu anchored to the popup HWND so the
                    // system knows which window owns it, then fire a one-shot
                    // poll so the file count is fresh.
                    use tray_icon::menu::ContextMenu;
                    unsafe {
                        self.tray_menu.menu.show_context_menu_for_hwnd(
                            self.popup.hwnd(),
                            None,
                        );
                    }
                    self.poller.poll_once();
                }
                _ => {}
            }
        }

        // Poll menu events.
        while let Ok(menu_event) = MenuEvent::receiver().try_recv() {
            self.handle_menu_event(&menu_event, event_loop);
        }

        if self.should_quit {
            event_loop.exit();
            return;
        }

        // Wake up every 100 ms so events feel responsive.
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(100),
        ));
    }
}

#[cfg(windows)]
impl TrayApp {
    fn toggle_popup(&mut self) {
        if self.popup.is_visible() {
            self.popup.hide();
            self.poller.set_active(false);
        } else {
            // Show current list immediately; it populates on the next poll.
            self.popup.update_files(&self.last_recent_files);
            self.popup.show();
            self.poller.set_active(true);
        }
    }

    fn handle_menu_event(
        &mut self,
        event: &MenuEvent,
        event_loop: &ActiveEventLoop,
    ) {
        if event.id == self.tray_menu.quit_id() {
            self.should_quit = true;
            event_loop.exit();
        } else if event.id == self.tray_menu.scan_id() {
            self.run_scan();
        } else if event.id == self.tray_menu.toggle_id() {
            self.toggle_service();
        } else if event.id == self.tray_menu.config_id() {
            self.open_config();
        }
    }

    fn run_scan(&self) {
        let scan_exe = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("find-scan.exe")))
            .unwrap_or_else(|| PathBuf::from("find-scan.exe"));

        if let Err(e) = std::process::Command::new(&scan_exe)
            .arg("--config")
            .arg(&self.config_path)
            .spawn()
        {
            show_error(
                "Find Anything — Scan Error",
                &format!("Failed to launch find-scan.exe:\n{e}"),
            );
        }
    }

    fn toggle_service(&self) {
        if self.service_running {
            if let Err(e) = service_ctl::stop_service() {
                show_error(
                    "Find Anything — Service Error",
                    &format!("Failed to stop the watcher service:\n{e}"),
                );
            }
        } else {
            if let Err(e) = service_ctl::start_service() {
                show_error(
                    "Find Anything — Service Error",
                    &format!(
                        "Failed to start the watcher service:\n{e}\n\n\
                         The service may not be installed. Try re-running the installer."
                    ),
                );
            }
        }
    }

    fn open_config(&self) {
        // ShellExecute "open" on the config file opens it in the default editor.
        use std::os::windows::ffi::OsStrExt;
        let path_wide: Vec<u16> = self
            .config_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let verb: Vec<u16> = "open\0".encode_utf16().collect();

        unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                0,
                verb.as_ptr(),
                path_wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
            );
        }
    }
}

#[cfg(windows)]
fn load_icon(bytes: &[u8]) -> Result<tray_icon::Icon> {
    // Decode the ICO file and use the first (largest) image as RGBA.
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Ico)
        .context("decoding ICO file")?;
    let img = img.into_rgba8();
    let (w, h) = img.dimensions();
    tray_icon::Icon::from_rgba(img.into_raw(), w, h).context("creating tray icon from RGBA")
}

#[cfg(windows)]
fn parse_config_arg() -> PathBuf {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--config" {
            if let Some(path) = args.next() {
                return PathBuf::from(path);
            }
        }
    }
    // Default config path for Windows.
    std::env::var_os("USERPROFILE")
        .map(|p| PathBuf::from(p).join(".config").join("FindAnything").join("client.toml"))
        .unwrap_or_else(|| PathBuf::from("client.toml"))
}
