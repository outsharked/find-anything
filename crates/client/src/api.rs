#![allow(dead_code)] // methods are used by different binaries in this crate

use anyhow::{Context, Result};
use flate2::{write::GzEncoder, Compression};
use reqwest::Client;
use std::io::Write;

use find_common::api::{
    AppSettingsResponse, BulkRequest, CompactResponse, ContextResponse, FileRecord,
    InboxDeleteResponse, InboxPauseResponse, InboxResumeResponse, InboxRetryResponse,
    InboxShowResponse, InboxStatusResponse, RecentFile, RecentResponse, SearchResponse,
    SourceDeleteResponse, SourceInfo, StatsResponse, StatsStreamEvent, UploadInitRequest,
    UploadInitResponse, UploadPatchResponse, UploadStatusResponse,
};

pub struct ApiClient {
    client: Client,
    base_url: String,
    token: String,
}

impl ApiClient {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// GET /api/v1/files?source=<name>  — returns existing (path, mtime) list.
    pub async fn list_files(&self, source: &str) -> Result<Vec<FileRecord>> {
        let resp = self
            .client
            .get(self.url("/api/v1/files"))
            .query(&[("source", source)])
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/files")?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        resp.error_for_status()
            .context("GET /api/v1/files status")?
            .json::<Vec<FileRecord>>()
            .await
            .context("parsing file list")
    }

    /// POST /api/v1/bulk  — upserts, deletions, and scan-complete in one request (gzip-compressed).
    pub async fn bulk(&self, req: &BulkRequest) -> Result<()> {
        let json = serde_json::to_vec(req).context("serialising bulk request")?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&json).context("compressing bulk request")?;
        let compressed = encoder.finish().context("finishing gzip stream")?;

        let resp = self.client
            .post(self.url("/api/v1/bulk"))
            .bearer_auth(&self.token)
            .header("Content-Encoding", "gzip")
            .header("Content-Type", "application/json")
            .body(compressed)
            .send()
            .await
            .context("POST /api/v1/bulk")?;

        let status = resp.status();
        if status == reqwest::StatusCode::ACCEPTED || status.is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("POST /api/v1/bulk: unexpected status {status}"))
        }
    }

    /// GET /api/v1/context
    pub async fn context(
        &self,
        source: &str,
        path: &str,
        archive_path: Option<&str>,
        line: usize,
        window: usize,
    ) -> Result<ContextResponse> {
        let mut req = self
            .client
            .get(self.url("/api/v1/context"))
            .bearer_auth(&self.token)
            .query(&[
                ("source", source),
                ("path", path),
                ("line", &line.to_string()),
                ("window", &window.to_string()),
            ]);
        if let Some(ap) = archive_path {
            req = req.query(&[("archive_path", ap)]);
        }
        req.send()
            .await
            .context("GET /api/v1/context")?
            .error_for_status()
            .context("context status")?
            .json::<ContextResponse>()
            .await
            .context("parsing context response")
    }

    /// GET /api/v1/stats
    pub async fn get_stats(&self, refresh: bool) -> Result<StatsResponse> {
        let url = if refresh {
            self.url("/api/v1/stats?refresh=true")
        } else {
            self.url("/api/v1/stats")
        };
        self.client
            .get(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/stats")?
            .error_for_status()
            .context("stats status")?
            .json::<StatsResponse>()
            .await
            .context("parsing stats response")
    }

    /// GET /api/v1/sources
    pub async fn get_sources(&self) -> Result<Vec<SourceInfo>> {
        self.client
            .get(self.url("/api/v1/sources"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/sources")?
            .error_for_status()
            .context("sources status")?
            .json::<Vec<SourceInfo>>()
            .await
            .context("parsing sources response")
    }

    /// GET /api/v1/settings
    pub async fn get_settings(&self) -> Result<AppSettingsResponse> {
        self.client
            .get(self.url("/api/v1/settings"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/settings")?
            .error_for_status()
            .context("settings status")?
            .json::<AppSettingsResponse>()
            .await
            .context("parsing settings response")
    }

    /// GET /api/v1/recent
    pub async fn get_recent(&self, limit: usize, sort_by_mtime: bool) -> Result<Vec<RecentFile>> {
        let sort = if sort_by_mtime { "mtime" } else { "indexed" };
        self.client
            .get(self.url(&format!("/api/v1/recent?limit={limit}&sort={sort}")))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/recent")?
            .error_for_status()
            .context("recent status")?
            .json::<RecentResponse>()
            .await
            .context("parsing recent response")
            .map(|r| r.files)
    }

    /// GET /api/v1/recent/stream — SSE stream of live activity events.
    ///
    /// Connects to the server-sent-events endpoint and calls `on_event` for
    /// each event received.  Runs until the connection drops or the returned
    /// future is cancelled (e.g. via `tokio::select!` with a ctrl-c branch).
    pub async fn stream_recent<F>(&self, limit: usize, sort_by_mtime: bool, mut on_event: F) -> Result<()>
    where
        F: FnMut(RecentFile),
    {
        let sort = if sort_by_mtime { "mtime" } else { "indexed" };
        let mut resp = self.client
            .get(self.url(&format!("/api/v1/recent/stream?limit={limit}&sort={sort}")))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/recent/stream")?
            .error_for_status()
            .context("recent/stream status")?;

        // Parse SSE frames: lines starting with "data:" separated by blank lines.
        let mut buf = Vec::<u8>::new();
        while let Some(chunk) = resp.chunk().await.context("reading SSE stream")? {
            buf.extend_from_slice(&chunk);
            // Process all complete events (terminated by \n\n).
            while let Some(pos) = find_double_newline(&buf) {
                if let Ok(event_str) = std::str::from_utf8(&buf[..pos]) {
                    for line in event_str.lines() {
                        if let Some(data) = line.strip_prefix("data:") {
                            if let Ok(file) = serde_json::from_str::<RecentFile>(data.trim()) {
                                on_event(file);
                            }
                        }
                    }
                }
                buf.drain(..pos + 2);
            }
        }
        Ok(())
    }

    /// Stream `GET /api/v1/stats/stream` (SSE).  Calls `on_event` for each snapshot.
    pub async fn stream_stats<F>(&self, mut on_event: F) -> Result<()>
    where
        F: FnMut(StatsStreamEvent),
    {
        let mut resp = self.client
            .get(self.url("/api/v1/stats/stream"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/stats/stream")?
            .error_for_status()
            .context("stats/stream status")?;

        let mut buf = Vec::<u8>::new();
        while let Some(chunk) = resp.chunk().await.context("reading stats SSE stream")? {
            buf.extend_from_slice(&chunk);
            while let Some(pos) = find_double_newline(&buf) {
                if let Ok(event_str) = std::str::from_utf8(&buf[..pos]) {
                    for line in event_str.lines() {
                        if let Some(data) = line.strip_prefix("data:") {
                            match serde_json::from_str::<StatsStreamEvent>(data.trim()) {
                                Ok(event) => on_event(event),
                                Err(e) => eprintln!("[stats stream] deserialize error: {e}"),
                            }
                        }
                    }
                }
                buf.drain(..pos + 2);
            }
        }
        Ok(())
    }

    /// GET /api/v1/admin/inbox
    pub async fn inbox_status(&self) -> Result<InboxStatusResponse> {
        self.client
            .get(self.url("/api/v1/admin/inbox"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("GET /api/v1/admin/inbox")?
            .error_for_status()
            .context("inbox status")?
            .json::<InboxStatusResponse>()
            .await
            .context("parsing inbox status response")
    }

    /// DELETE /api/v1/admin/inbox?target=<target>
    pub async fn inbox_clear(&self, target: &str) -> Result<InboxDeleteResponse> {
        self.client
            .delete(self.url("/api/v1/admin/inbox"))
            .bearer_auth(&self.token)
            .query(&[("target", target)])
            .send()
            .await
            .context("DELETE /api/v1/admin/inbox")?
            .error_for_status()
            .context("inbox clear status")?
            .json::<InboxDeleteResponse>()
            .await
            .context("parsing inbox delete response")
    }

    /// GET /api/v1/admin/inbox/show?name=<name>
    pub async fn inbox_show(&self, name: &str) -> Result<Option<InboxShowResponse>> {
        let resp = self.client
            .get(self.url("/api/v1/admin/inbox/show"))
            .bearer_auth(&self.token)
            .query(&[("name", name)])
            .send()
            .await
            .context("GET /api/v1/admin/inbox/show")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Ok(Some(
            resp.error_for_status()
                .context("inbox show")?
                .json::<InboxShowResponse>()
                .await
                .context("parsing inbox show response")?,
        ))
    }

    /// DELETE /api/v1/admin/source?source=<name>
    pub async fn delete_source(&self, source: &str) -> Result<SourceDeleteResponse> {
        let resp = self
            .client
            .delete(self.url("/api/v1/admin/source"))
            .bearer_auth(&self.token)
            .query(&[("source", source)])
            .send()
            .await
            .context("DELETE /api/v1/admin/source")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!("source '{}' not found", source);
        }
        resp.error_for_status()
            .context("delete source status")?
            .json::<SourceDeleteResponse>()
            .await
            .context("parsing delete source response")
    }

    /// POST /api/v1/admin/inbox/retry
    pub async fn inbox_retry(&self) -> Result<InboxRetryResponse> {
        self.client
            .post(self.url("/api/v1/admin/inbox/retry"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("POST /api/v1/admin/inbox/retry")?
            .error_for_status()
            .context("inbox retry status")?
            .json::<InboxRetryResponse>()
            .await
            .context("parsing inbox retry response")
    }

    /// POST /api/v1/admin/compact
    pub async fn compact(&self, dry_run: bool) -> Result<CompactResponse> {
        let url = if dry_run {
            self.url("/api/v1/admin/compact?dry_run=true")
        } else {
            self.url("/api/v1/admin/compact")
        };
        self.client
            .post(url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("POST /api/v1/admin/compact")?
            .error_for_status()
            .context("compact status")?
            .json::<CompactResponse>()
            .await
            .context("parsing compact response")
    }

    /// POST /api/v1/admin/inbox/pause
    pub async fn inbox_pause(&self) -> Result<InboxPauseResponse> {
        self.client
            .post(self.url("/api/v1/admin/inbox/pause"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("POST /api/v1/admin/inbox/pause")?
            .error_for_status()
            .context("inbox pause status")?
            .json::<InboxPauseResponse>()
            .await
            .context("parsing inbox pause response")
    }

    /// POST /api/v1/admin/inbox/resume
    pub async fn inbox_resume(&self) -> Result<InboxResumeResponse> {
        self.client
            .post(self.url("/api/v1/admin/inbox/resume"))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("POST /api/v1/admin/inbox/resume")?
            .error_for_status()
            .context("inbox resume status")?
            .json::<InboxResumeResponse>()
            .await
            .context("parsing inbox resume response")
    }

    /// POST /api/v1/upload — initiate a resumable upload.
    pub async fn upload_init(
        &self,
        source: &str,
        rel_path: &str,
        mtime: i64,
        size: u64,
    ) -> Result<UploadInitResponse> {
        let req = UploadInitRequest {
            source: source.to_string(),
            rel_path: rel_path.to_string(),
            mtime,
            size,
        };
        self.client
            .post(self.url("/api/v1/upload"))
            .bearer_auth(&self.token)
            .json(&req)
            .send()
            .await
            .context("POST /api/v1/upload")?
            .error_for_status()
            .context("upload init status")?
            .json::<UploadInitResponse>()
            .await
            .context("parsing upload init response")
    }

    /// PATCH /api/v1/upload/{id} — send a chunk.
    pub async fn upload_patch(
        &self,
        upload_id: &str,
        content_range: &str,
        data: Vec<u8>,
    ) -> Result<UploadPatchResponse> {
        self.client
            .patch(self.url(&format!("/api/v1/upload/{upload_id}")))
            .bearer_auth(&self.token)
            .header("Content-Range", content_range)
            .header("Content-Type", "application/octet-stream")
            .body(data)
            .send()
            .await
            .context("PATCH /api/v1/upload")?
            .error_for_status()
            .context("upload patch status")?
            .json::<UploadPatchResponse>()
            .await
            .context("parsing upload patch response")
    }

    /// HEAD /api/v1/upload/{id} — query upload progress.
    pub async fn upload_status(&self, upload_id: &str) -> Result<UploadStatusResponse> {
        self.client
            .head(self.url(&format!("/api/v1/upload/{upload_id}")))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("HEAD /api/v1/upload")?
            .error_for_status()
            .context("upload status")?
            .json::<UploadStatusResponse>()
            .await
            .context("parsing upload status response")
    }

    /// Check that this client meets the server's minimum version requirement.
    /// Returns an error with a human-readable message if the client is too old.
    /// Silently succeeds if the server does not advertise a minimum version or
    /// if the version strings cannot be parsed (fail-open).
    pub async fn check_server_version(&self) -> Result<()> {
        let settings = self.get_settings().await
            .context("fetching server settings for version check")?;
        let client_ver = env!("CARGO_PKG_VERSION");
        let min_ver = &settings.min_client_version;
        if !version_meets_minimum(client_ver, min_ver) {
            anyhow::bail!(
                "client version {client_ver} is too old — server requires >= {min_ver}.\n\
                 Please upgrade find-anything."
            );
        }
        Ok(())
    }

    /// GET /api/v1/search
    pub async fn search(
        &self,
        query: &str,
        mode: &str,
        sources: &[String],
        limit: usize,
        offset: usize,
    ) -> Result<SearchResponse> {
        let mut req = self
            .client
            .get(self.url("/api/v1/search"))
            .bearer_auth(&self.token)
            .query(&[
                ("q", query),
                ("mode", mode),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
            ]);
        for s in sources {
            req = req.query(&[("source", s.as_str())]);
        }
        req.send()
            .await
            .context("GET /api/v1/search")?
            .error_for_status()
            .context("search status")?
            .json::<SearchResponse>()
            .await
            .context("parsing search response")
    }
}

/// Returns true if `client_ver` satisfies `>= min_ver` using semver ordering.
/// Fails open (returns true) if either string cannot be parsed.
fn version_meets_minimum(client_ver: &str, min_ver: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut parts = v.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.parse().ok()?;
        Some((major, minor, patch))
    }
    match (parse(client_ver), parse(min_ver)) {
        (Some(c), Some(m)) => c >= m,
        _ => true,
    }
}

/// Find the position of the first `\n\n` sequence in `buf`.
fn find_double_newline(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}
