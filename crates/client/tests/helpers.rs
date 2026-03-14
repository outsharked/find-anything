#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use axum::serve;
use find_common::api::{FileRecord, SearchResult, StatsResponse};
use find_common::config::{
    ClientConfig, ServerConfig, ScanConfig, SourceConfig, WatchConfig,
};
use find_server::{build_router, create_app_state};
use find_common::config::parse_server_config;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use tokio::net::TcpListener;

use find_client::api::ApiClient;

pub const TEST_TOKEN: &str = "integration-test-token";
pub const TEST_SOURCE: &str = "test-source";

/// Path to the `target/debug/` directory, resolved from this crate's manifest.
/// Used to locate built-in extractor binaries (`find-extract-*`) during tests.
pub fn target_debug_dir() -> String {
    // CARGO_MANIFEST_DIR = crates/client; go up two levels to workspace root.
    let manifest = env!("CARGO_MANIFEST_DIR");
    let workspace = Path::new(manifest).join("..").join("..");
    workspace
        .join("target")
        .join("debug")
        .canonicalize()
        .unwrap_or_else(|_| workspace.join("target").join("debug"))
        .to_string_lossy()
        .to_string()
}

/// Path to the fixtures directory for this crate.
pub fn fixtures_dir() -> String {
    let manifest = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest)
        .join("tests")
        .join("fixtures")
        .to_string_lossy()
        .to_string()
}

pub struct TestServer {
    pub base_url: String,
    pub client: reqwest::Client,
    _data_dir: tempfile::TempDir,
}

impl TestServer {
    pub async fn spawn() -> Self {
        let data_dir = tempfile::TempDir::new().expect("tempdir");
        let data_path = data_dir.path().to_str().unwrap().to_string();

        let config_toml = format!(
            "[server]\ndata_dir = \"{data_path}\"\ntoken = \"{TEST_TOKEN}\"\n"
        );
        let (config, _) = parse_server_config(&config_toml).expect("parse config");

        let state = create_app_state(config).await.expect("create_app_state");
        let app = build_router(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        tokio::spawn(async move {
            serve(listener, app).await.expect("serve");
        });

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TEST_TOKEN}")).unwrap(),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("reqwest client");

        TestServer {
            base_url: format!("http://{addr}"),
            client,
            _data_dir: data_dir,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    pub async fn wait_for_idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let resp: StatsResponse = self
                .client
                .get(self.url("/api/v1/stats"))
                .send()
                .await
                .expect("stats request")
                .json()
                .await
                .expect("stats json");

            if resp.inbox_pending == 0 && resp.archive_queue == 0 {
                return;
            }
            if Instant::now() >= deadline {
                panic!(
                    "worker did not become idle within 10s (inbox_pending={}, archive_queue={})",
                    resp.inbox_pending, resp.archive_queue
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Full test environment: server + source directory.
pub struct TestEnv {
    pub server: TestServer,
    pub source_dir: tempfile::TempDir,
    pub source_name: String,
}

impl TestEnv {
    pub async fn new() -> Self {
        let server = TestServer::spawn().await;
        let source_dir = tempfile::TempDir::new().expect("source tempdir");
        Self {
            server,
            source_dir,
            source_name: TEST_SOURCE.to_string(),
        }
    }

    /// Write a file relative to source_dir and return its absolute path.
    pub fn write_file(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.source_dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(&path, content).expect("write file");
        path
    }

    /// Write binary content to a file relative to source_dir.
    pub fn write_file_bytes(&self, rel: &str, content: &[u8]) -> PathBuf {
        let path = self.source_dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create dirs");
        }
        std::fs::write(&path, content).expect("write binary file");
        path
    }

    /// Delete a file relative to source_dir.
    pub fn remove_file(&self, rel: &str) {
        std::fs::remove_file(self.source_dir.path().join(rel)).expect("remove file");
    }

    /// Build a ScanConfig for tests, with default settings.
    pub fn scan_config(&self) -> ScanConfig {
        ScanConfig {
            extractor_dir: Some(target_debug_dir()),
            ..ScanConfig::default()
        }
    }

    /// Build a ScanConfig with a custom mutator applied.
    pub fn scan_config_with<F: FnOnce(&mut ScanConfig)>(&self, f: F) -> ScanConfig {
        let mut cfg = self.scan_config();
        f(&mut cfg);
        cfg
    }

    /// Build an ApiClient connected to the test server.
    pub fn api_client(&self) -> ApiClient {
        ApiClient::new(&self.server.base_url, TEST_TOKEN)
    }

    /// Build a ClientConfig pointing at source_dir (used for watch tests).
    pub fn client_config(&self) -> ClientConfig {
        self.client_config_with(|_| {})
    }

    pub fn client_config_with<F: FnOnce(&mut WatchConfig)>(&self, f: F) -> ClientConfig {
        let mut watch = WatchConfig {
            scan_interval_hours: 0.0, // disable periodic find-scan subprocess
            ..WatchConfig::default()
        };
        f(&mut watch);
        ClientConfig {
            server: ServerConfig {
                url: self.server.base_url.clone(),
                token: TEST_TOKEN.to_string(),
            },
            sources: vec![SourceConfig {
                name: self.source_name.clone(),
                path: self.source_dir.path().to_string_lossy().to_string(),
                include: vec![],
            }],
            scan: self.scan_config(),
            watch,
            log: Default::default(),
            tray: Default::default(),
            cli: Default::default(),
        }
    }

    /// Run find-scan over source_dir and wait for the server to finish processing.
    pub async fn run_scan(&self) {
        self.run_scan_with(self.scan_config()).await;
    }

    /// Run find-scan with a specific ScanConfig.
    pub async fn run_scan_with(&self, scan: ScanConfig) {
        let api = self.api_client();
        let paths = vec![self.source_dir.path().to_string_lossy().to_string()];
        let source = find_client::scan::ScanSource {
            name: &self.source_name,
            paths: &paths,
            include: &[],
            subdir: None,
        };
        let opts = find_client::scan::ScanOptions {
            upgrade: false,
            quiet: true,
            dry_run: false,
            force_since: None,
        };
        find_client::scan::run_scan(&api, &source, &scan, &opts)
            .await
            .expect("run_scan failed");
        self.server.wait_for_idle().await;
    }

    /// Search via the server API and return results.
    pub async fn search(&self, query: &str) -> Vec<SearchResult> {
        let api = self.api_client();
        api.search(query, "fts", &[self.source_name.clone()], 50, 0)
            .await
            .expect("search failed")
            .results
    }

    /// List all indexed files for the test source.
    pub async fn list_files(&self) -> Vec<FileRecord> {
        let api = self.api_client();
        api.list_files(&self.source_name)
            .await
            .expect("list_files failed")
    }
}
