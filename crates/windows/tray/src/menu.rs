//! Tray icon menu construction and dynamic label updates.

use tray_icon::menu::{Menu, MenuId, MenuItem, PredefinedMenuItem};

/// Holds references to menu items that need runtime text updates.
pub struct TrayMenu {
    pub menu: Menu,
    pub status_item: MenuItem,
    pub filecount_item: MenuItem,
    pub scan_item: MenuItem,
    pub toggle_item: MenuItem,
    pub config_item: MenuItem,
    pub quit_item: MenuItem,
}

impl TrayMenu {
    pub fn new() -> anyhow::Result<Self> {
        let menu = Menu::new();

        // Disabled informational labels at the top.
        let status_item = MenuItem::new("Watcher: Unknown", false, None);
        let filecount_item = MenuItem::new("Connecting to server\u{2026}", false, None);

        // Action items.
        let scan_item = MenuItem::new("Run Full Scan", true, None);
        let toggle_item = MenuItem::new("Stop Watcher", true, None);
        let config_item = MenuItem::new("Open Config File", true, None);
        let quit_item = MenuItem::new("Quit Tray", true, None);

        menu.append(&status_item)?;
        menu.append(&filecount_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&scan_item)?;
        menu.append(&toggle_item)?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&config_item)?;
        menu.append(&quit_item)?;

        Ok(Self {
            menu,
            status_item,
            filecount_item,
            scan_item,
            toggle_item,
            config_item,
            quit_item,
        })
    }

    /// Show an interim transitioning state while the SCM command is in flight.
    /// Disables the toggle button until the next status poll confirms the new state.
    pub fn update_pending(&self, stopping: bool) {
        let status_text = if stopping {
            "Watcher: Stopping\u{2026}"
        } else {
            "Watcher: Starting\u{2026}"
        };
        self.status_item.set_text(status_text);
        self.toggle_item.set_enabled(false);
    }

    /// Returns the MenuId of each action item for event matching.
    pub fn scan_id(&self) -> MenuId { self.scan_item.id().clone() }
    pub fn toggle_id(&self) -> MenuId { self.toggle_item.id().clone() }
    pub fn config_id(&self) -> MenuId { self.config_item.id().clone() }
    pub fn quit_id(&self) -> MenuId { self.quit_item.id().clone() }

    /// Update the status labels and toggle button text based on service state
    /// and server file count.  Always re-enables the toggle button so that a
    /// previous `update_pending` call is cleared once the real state arrives.
    pub fn update_status(&self, service_running: bool, file_count: Option<u64>, source_count: Option<usize>) {
        let status_text = if service_running {
            "Watcher: Running"
        } else {
            "Watcher: Stopped"
        };
        self.status_item.set_text(status_text);

        let toggle_text = if service_running {
            "Stop Watcher"
        } else {
            "Start Watcher"
        };
        self.toggle_item.set_enabled(true);
        self.toggle_item.set_text(toggle_text);

        let count_text = match (file_count, source_count) {
            (Some(fc), Some(sc)) => format!("{} files across {} source(s)", format_num(fc), sc),
            _ => "Connecting to server\u{2026}".to_string(),
        };
        self.filecount_item.set_text(&count_text);
    }
}

fn format_num(n: u64) -> String {
    let s = n.to_string();
    let digits: Vec<char> = s.chars().collect();
    let mut result = String::new();
    let len = digits.len();
    for (i, &c) in digits.iter().enumerate() {
        result.push(c);
        let remaining = len - i - 1;
        if remaining > 0 && remaining % 3 == 0 {
            result.push(',');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn format_num_basic() {
        assert_eq!(format_num(0), "0");
        assert_eq!(format_num(999), "999");
        assert_eq!(format_num(1000), "1,000");
        assert_eq!(format_num(42153), "42,153");
        assert_eq!(format_num(1_000_000), "1,000,000");
    }
}
