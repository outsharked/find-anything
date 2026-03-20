#![allow(dead_code)]

use std::time::{Duration, Instant};

use find_common::api::{BulkRequest, FileKind, IndexFile, IndexLine, StatsResponse, SCANNER_VERSION};
use find_common::config::parse_server_config;
use find_server::{build_router, create_app_state};
use flate2::{write::GzEncoder, Compression};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use std::io::Write;
use tokio::net::TcpListener;

pub const TEST_TOKEN: &str = "integration-test-token";

pub struct TestServer {
    pub base_url: String,
    pub client: reqwest::Client,
    _data_dir: tempfile::TempDir,
}

impl TestServer {
    pub async fn spawn() -> Self {
        Self::spawn_with_extra_config("").await
    }

    /// Spawn a TestServer with additional TOML config appended (e.g. source path config).
    pub async fn spawn_with_extra_config(extra: &str) -> Self {
        let data_dir = tempfile::TempDir::new().expect("tempdir");
        let data_path = data_dir.path().to_str().unwrap().to_string();

        let config_toml = format!(
            "[server]\ndata_dir = \"{data_path}\"\ntoken = \"{TEST_TOKEN}\"\n{extra}"
        );
        let (config, _) = parse_server_config(&config_toml).expect("parse config");

        let state = create_app_state(config).await.expect("create_app_state");
        let app = build_router(state);

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");

        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("serve");
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

    /// Returns a URL for the given path.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base_url)
    }

    /// Poll GET /api/v1/stats until both inbox_pending and archive_queue are 0.
    /// Panics after 10 seconds.
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
                panic!("worker did not become idle within 10s (inbox_pending={}, archive_queue={})",
                    resp.inbox_pending, resp.archive_queue);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// GET /api/v1/stats (from cache, no refresh).
    pub async fn get_stats(&self) -> StatsResponse {
        self.client
            .get(self.url("/api/v1/stats"))
            .send()
            .await
            .expect("stats request")
            .json()
            .await
            .expect("stats json")
    }

    /// GET /api/v1/stats?refresh=true (forces a DB rebuild of the cache).
    pub async fn get_stats_refresh(&self) -> StatsResponse {
        self.client
            .get(self.url("/api/v1/stats?refresh=true"))
            .send()
            .await
            .expect("stats refresh request")
            .json()
            .await
            .expect("stats refresh json")
    }

    /// gzip-encode a BulkRequest and POST to /api/v1/bulk; asserts 202.
    pub fn data_dir_path(&self) -> &std::path::Path {
        self._data_dir.path()
    }

    pub async fn post_bulk(&self, req: &BulkRequest) {
        let json = serde_json::to_vec(req).expect("serialize bulk");
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&json).expect("gzip write");
        let gz = enc.finish().expect("gzip finish");

        let status = self
            .client
            .post(self.url("/api/v1/bulk"))
            .header("Content-Encoding", "gzip")
            .header("Content-Type", "application/json")
            .body(gz)
            .send()
            .await
            .expect("bulk request")
            .status();

        assert_eq!(status.as_u16(), 202, "expected 202 from /api/v1/bulk");
    }
}

/// Build a BulkRequest identical to `make_text_bulk` but with a file_hash set,
/// so the archive worker writes chunks to the content store (it skips files with `file_hash: None`).
pub fn make_text_bulk_hashed(source: &str, path: &str, content: &str) -> BulkRequest {
    let mut req = make_text_bulk(source, path, content);
    req.files[0].file_hash = Some(
        "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
    );
    req
}

/// Write a minimal valid gzip file to `path` (used to seed the failed dir in tests).
pub fn write_fake_gz(path: &std::path::Path) {
    let file = std::fs::File::create(path).unwrap();
    let mut enc = GzEncoder::new(file, Compression::default());
    enc.write_all(b"{}").unwrap();
    enc.finish().unwrap();
}

/// Build a BulkRequest that indexes a single plain-text file.
/// line_number=0 is the filename; content lines start at 1.
/// Includes a deterministic file_hash so the archive phase stores content in
/// the content store (required for context retrieval).
pub fn make_text_bulk(source: &str, path: &str, content: &str) -> BulkRequest {
    use find_common::api::{LINE_PATH, LINE_METADATA, LINE_CONTENT_START};
    let mut lines = vec![
        IndexLine {
            archive_path: None,
            line_number: LINE_PATH,
            content: format!("[PATH] {path}"),
        },
        IndexLine {
            archive_path: None,
            line_number: LINE_METADATA,
            content: String::new(),
        },
    ];
    for (i, line) in content.lines().enumerate() {
        lines.push(IndexLine {
            archive_path: None,
            line_number: i + LINE_CONTENT_START,
            content: line.to_string(),
        });
    }

    BulkRequest {
        source: source.to_string(),
        files: vec![IndexFile {
            path: path.to_string(),
            mtime: 1_700_000_000,
            size: Some(content.len() as i64),
            kind: FileKind::Text,
            lines,
            extract_ms: None,
            file_hash: Some(fnv_hash_hex(path, content)),
            scanner_version: SCANNER_VERSION,
            is_new: true,
        }],
        delete_paths: vec![],
        scan_timestamp: Some(1_700_000_000),
        indexing_failures: vec![],
        rename_paths: vec![],
    }
}

/// Compute a deterministic 64-char hex "hash" of (path, content) using FNV-1a.
/// Not cryptographically secure, but stable and unique enough for tests.
fn fnv_hash_hex(path: &str, content: &str) -> String {
    const FNV_PRIME: u64 = 1099511628211;
    const FNV_BASIS: u64 = 14695981039346656037;
    let mut h = FNV_BASIS;
    for b in path.bytes().chain(std::iter::once(b'|')).chain(content.bytes()) {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    // Repeat the 16-hex-digit value 4× to mimic a 64-char blake3/sha256 hash.
    format!("{h:016x}{h:016x}{h:016x}{h:016x}")
}
