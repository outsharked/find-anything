//! Background thread that polls the Windows SCM for service state and the
//! find-anything server for file counts and recent files.
//!
//! Polling is demand-driven: the thread is idle (sleeping 100 ms) when the
//! popup is closed, and active (polling every `poll_interval_ms`) while it is
//! open.  A one-shot poll can also be requested for right-click menu refresh.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use find_common::api::RecentFile;

use crate::AppEvent;
use crate::service_ctl;

const IDLE_SLEEP_MS: u64 = 100;

/// Handle returned by [`spawn`]; lets the main thread control polling.
pub struct PollerHandle {
    active: Arc<AtomicBool>,
    poll_once: Arc<AtomicBool>,
}

impl PollerHandle {
    /// Enable or disable continuous polling (used while the popup is open).
    pub fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Relaxed);
    }

    /// Request a single immediate poll (used on right-click to freshen counts).
    pub fn poll_once(&self) {
        self.poll_once.store(true, Ordering::Relaxed);
    }
}

/// Spawn the background poller thread and return a handle to control it.
pub fn spawn(
    tx: Sender<AppEvent>,
    server_url: String,
    token: String,
    poll_interval_ms: u64,
) -> PollerHandle {
    let active = Arc::new(AtomicBool::new(false));
    let poll_once = Arc::new(AtomicBool::new(false));

    let active_clone = Arc::clone(&active);
    let poll_once_clone = Arc::clone(&poll_once);

    thread::Builder::new()
        .name("find-tray-poller".into())
        .spawn(move || {
            run(tx, server_url, token, poll_interval_ms, active_clone, poll_once_clone)
        })
        .expect("spawning poller thread");

    PollerHandle { active, poll_once }
}

fn run(
    tx: Sender<AppEvent>,
    server_url: String,
    token: String,
    poll_interval_ms: u64,
    active: Arc<AtomicBool>,
    poll_once: Arc<AtomicBool>,
) {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default();

    loop {
        let is_active = active.load(Ordering::Relaxed);
        let do_once = poll_once.swap(false, Ordering::Relaxed);

        if is_active || do_once {
            let service_running = service_ctl::is_service_running();
            let (file_count, source_count) = query_status(&client, &server_url, &token);
            let recent_files = query_recent(&client, &server_url, &token);

            let event = AppEvent::StatusUpdate {
                service_running,
                file_count,
                source_count,
                recent_files,
            };

            if tx.send(event).is_err() {
                break;
            }

            if is_active {
                thread::sleep(Duration::from_millis(poll_interval_ms));
            } else {
                // One-shot poll finished; back to idle check cadence.
                thread::sleep(Duration::from_millis(IDLE_SLEEP_MS));
            }
        } else {
            thread::sleep(Duration::from_millis(IDLE_SLEEP_MS));
        }
    }
}

fn query_status(
    client: &reqwest::blocking::Client,
    server_url: &str,
    token: &str,
) -> (Option<u64>, Option<usize>) {
    let url = format!("{server_url}/api/v1/stats");
    let resp = match client.get(&url).bearer_auth(token).send() {
        Ok(r) => r,
        Err(_) => return (None, None),
    };

    if !resp.status().is_success() {
        return (None, None);
    }

    let json: serde_json::Value = match resp.json() {
        Ok(v) => v,
        Err(_) => return (None, None),
    };

    if let Some(sources) = json.get("sources").and_then(|v| v.as_array()) {
        let total_files: u64 = sources
            .iter()
            .filter_map(|s| s.get("total_files").and_then(|v| v.as_u64()))
            .sum();
        (Some(total_files), Some(sources.len()))
    } else {
        (None, None)
    }
}

fn query_recent(
    client: &reqwest::blocking::Client,
    server_url: &str,
    token: &str,
) -> Vec<RecentFile> {
    let url = format!("{server_url}/api/v1/recent?limit=50");
    let resp = match client.get(&url).bearer_auth(token).send() {
        Ok(r) => r,
        Err(_) => return vec![],
    };

    if !resp.status().is_success() {
        return vec![];
    }

    resp.json::<find_common::api::RecentResponse>()
        .map(|r| r.files)
        .unwrap_or_default()
}
