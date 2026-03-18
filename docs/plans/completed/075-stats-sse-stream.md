# Stats SSE Stream (plan 075)

**Goal:** Add `GET /api/v1/stats/stream` — an SSE endpoint that pushes stats snapshots whenever the cache changes, rate-limited to a configurable max (default 5 events/sec). Replace the polling loop in `find-admin status --watch` with SSE consumption.

---

## Design

### Rate limiting via `tokio::sync::watch`

`watch::Sender<u64>` (a sequence counter) sits in `AppState`. Workers and rebuilds call `stats_watch.send(seq + 1)` after every cache update. Each SSE subscriber holds a `watch::Receiver<u64>` and uses this loop:

```
send initial snapshot
loop:
    rx.changed().await          // edge-triggered; coalesces rapid fires
    throttle sleep if needed    // enforces max rate
    rx.borrow_and_update()      // mark seen so next changed() waits for new write
    read cache snapshot
    send SSE event
```

`watch` naturally coalesces: 100 worker batches in 200ms produce one `changed()` wake, not 100. The throttle sleep only adds latency if the server is slower than the rate limit.

### Snapshot content

The SSE snapshot is built from the in-memory cache only — no DB opens. It omits `history` (doesn't change during a scan) and `last_scan`/`indexing_error_count` (irrelevant to a live counter display). A new `StatsStreamEvent` type carries only what's useful at high frequency:

```rust
pub struct StatsStreamEvent {
    pub sources: Vec<SourceStreamSnapshot>,
}
pub struct SourceStreamSnapshot {
    pub name:          String,
    pub total_files:   usize,
    pub total_size:    i64,
    pub by_kind:       HashMap<FileKind, KindStats>,
    pub fts_row_count: i64,
}
```

This is the only new API type needed. The existing `StatsResponse` is unchanged and still served by `GET /api/v1/stats`.

### Client SSE parsing

No new crate. Add the `stream` feature to the client's `reqwest`. Read the response as a byte stream, split on `\n\n`, extract `data:` lines, deserialize JSON. ~25 lines of code.

### Watch mode UX

`find-admin status --watch` connects to the stream endpoint. On each event it clears and redraws using the existing `format_status`-style rendering (adapted to `StatsStreamEvent`). Ctrl-C terminates via `tokio::select!` as before.

---

## Config

Add to `ServerConfig` in `crates/common/src/config.rs`:

```toml
[server]
stats_stream_rate_hz = 5.0   # max SSE events per second (default: 5.0)
```

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/api.rs` | Add `StatsStreamEvent`, `SourceStreamSnapshot` types |
| `crates/common/src/config.rs` | Add `stats_stream_rate_hz: f64` to `ServerConfig` |
| `crates/server/src/lib.rs` | Add `stats_watch: Arc<watch::Sender<u64>>` to `AppState` |
| `crates/server/src/worker/request.rs` | Increment `stats_watch` after `apply_delta` in success arm |
| `crates/server/src/stats_cache.rs` | `full_rebuild` returns `()`, callers increment `stats_watch` after it returns |
| `crates/server/src/routes/stats.rs` | Add `stream_stats` handler |
| `crates/server/src/routes/mod.rs` | Register `GET /api/v1/stats/stream` |
| `crates/client/Cargo.toml` | Add `stream` feature to `reqwest` |
| `crates/client/src/api.rs` | Add `stream_stats()` returning an async byte stream |
| `crates/client/src/admin_main.rs` | `--watch` uses SSE instead of poll loop |

---

## Task 1: API types + config

**Files:** `crates/common/src/api.rs`, `crates/common/src/config.rs`

- [ ] Add to `api.rs`:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct StatsStreamEvent {
      pub sources: Vec<SourceStreamSnapshot>,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct SourceStreamSnapshot {
      pub name:          String,
      pub total_files:   usize,
      pub total_size:    i64,
      pub by_kind:       HashMap<FileKind, KindStats>,
      pub fts_row_count: i64,
  }
  ```

- [ ] Add to `ServerConfig` in `config.rs`:
  ```rust
  #[serde(default = "default_stats_stream_rate_hz")]
  pub stats_stream_rate_hz: f64,
  ```
  with `fn default_stats_stream_rate_hz() -> f64 { 5.0 }` and add to `Default` impl.

- [ ] `cargo check --workspace`

---

## Task 2: `stats_watch` channel in `AppState`

**Files:** `crates/server/src/lib.rs`, `crates/server/src/worker/request.rs`

- [ ] In `lib.rs`, add to `AppState`:
  ```rust
  pub stats_watch: Arc<tokio::sync::watch::Sender<u64>>,
  ```
  Initialize with `let (stats_watch_tx, _) = tokio::sync::watch::channel(0u64);` and store `Arc::new(stats_watch_tx)`.

- [ ] In `worker/request.rs`, in the `Ok(Ok(Ok(delta)))` success arm, after `guard.apply_delta(&delta)`:
  ```rust
  let _ = handles.stats_watch.send_modify(|v| *v = v.wrapping_add(1));
  ```
  Add `stats_watch: Arc<tokio::sync::watch::Sender<u64>>` to `IndexerHandles`. Thread it from `WorkerHandles` → `start_inbox_worker` → `IndexerHandles`.

- [ ] Add `stats_watch` to `WorkerHandles` in `worker/mod.rs`. Wire from `create_app_state` in `lib.rs`.

- [ ] Fire from the three `full_rebuild` call sites (startup in `lib.rs`, daily in `compaction.rs`, refresh in `routes/stats.rs`) — after `full_rebuild` returns:
  ```rust
  let _ = state.stats_watch.send_modify(|v| *v = v.wrapping_add(1));
  ```
  The compaction call site needs access to `stats_watch` — pass `Arc::clone(&state.stats_watch)` into `start_compaction_scanner` (add a parameter).

- [ ] `cargo check -p find-server` + `mise run clippy`

---

## Task 3: `GET /api/v1/stats/stream` SSE endpoint

**Files:** `crates/server/src/routes/stats.rs`, `crates/server/src/routes/mod.rs`

- [ ] Add `stream_stats` handler:

```rust
pub async fn stream_stats(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(s) = check_auth(&state, &headers) {
        return (s, "Unauthorized").into_response();
    }

    let rate_hz = state.config.server.stats_stream_rate_hz.max(0.1);
    let min_interval = Duration::from_secs_f64(1.0 / rate_hz);

    let mut rx = state.stats_watch.subscribe();
    let state_clone = Arc::clone(&state);

    let stream = async_stream::stream! {
        // Send initial snapshot immediately.
        yield build_stream_event(&state_clone);

        let mut last_sent = std::time::Instant::now();

        loop {
            // Wait for any change; coalesces rapid updates.
            if rx.changed().await.is_err() { break; }

            // Throttle: sleep enough to honour the rate limit.
            let elapsed = last_sent.elapsed();
            if elapsed < min_interval {
                tokio::time::sleep(min_interval - elapsed).await;
            }

            // Mark current value seen so next changed() waits for a new write.
            rx.borrow_and_update();

            yield build_stream_event(&state_clone);
            last_sent = std::time::Instant::now();
        }
    };

    let sse_stream = stream.map(|event: StatsStreamEvent| {
        Ok::<Event, std::convert::Infallible>(
            Event::default().json_data(&event).unwrap_or_default()
        )
    });

    Sse::new(sse_stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

fn build_stream_event(state: &AppState) -> StatsStreamEvent {
    let guard = state.source_stats_cache.read().unwrap_or_else(|e| e.into_inner());
    StatsStreamEvent {
        sources: guard.sources.iter().map(|s| SourceStreamSnapshot {
            name:          s.name.clone(),
            total_files:   s.total_files,
            total_size:    s.total_size,
            by_kind:       s.by_kind.clone(),
            fts_row_count: s.fts_row_count,
        }).collect(),
    }
}
```

Note: `async_stream` crate is already used elsewhere in the server, or replace with a manual `Stream` impl using `futures::stream::unfold` if not available. Check `Cargo.toml` first.

- [ ] Register in `routes/mod.rs`:
  ```rust
  .route("/api/v1/stats/stream", get(stats::stream_stats))
  ```

- [ ] `cargo check -p find-server` + `mise run clippy`

---

## Task 4: Client SSE consumption + `--watch` via stream

**Files:** `crates/client/Cargo.toml`, `crates/client/src/api.rs`, `crates/client/src/admin_main.rs`

- [ ] Add `stream` feature to reqwest in `crates/client/Cargo.toml`:
  ```toml
  reqwest = { version = "0.13", features = ["json", "rustls", "query", "stream"], default-features = false }
  ```

- [ ] Add `stream_stats` to `api.rs` — returns a `impl Stream<Item = StatsStreamEvent>`:

```rust
pub async fn stream_stats(&self) -> anyhow::Result<impl futures::Stream<Item = StatsStreamEvent>> {
    let resp = self.client
        .get(format!("{}/api/v1/stats/stream", self.base_url))
        .header("Authorization", format!("Bearer {}", self.token))
        .send().await?;

    let byte_stream = resp.bytes_stream();

    // SSE format: lines like "data: {...}\n\n"
    // Buffer incomplete lines across chunks.
    let stream = async_stream::stream! {
        use futures::StreamExt as _;
        let mut buf = String::new();
        tokio::pin!(byte_stream);
        while let Some(chunk) = byte_stream.next().await {
            let Ok(bytes) = chunk else { break };
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find("\n\n") {
                let block = buf[..pos].to_string();
                buf = buf[pos + 2..].to_string();
                for line in block.lines() {
                    if let Some(json) = line.strip_prefix("data: ") {
                        if let Ok(event) = serde_json::from_str::<StatsStreamEvent>(json) {
                            yield event;
                        }
                    }
                }
            }
        }
    };
    Ok(stream)
}
```

- [ ] In `admin_main.rs`, replace the `--watch` poll loop:

```rust
// Old: loop { get_stats(); sleep(poll) }
// New:
let mut stream = client.stream_stats().await.context("connecting to stats stream")?;
tokio::pin!(stream);
loop {
    tokio::select! {
        event = stream.next() => {
            match event {
                Some(e) => {
                    let output = format_stream_status(&e);
                    print!("\x1b[2J\x1b[H{output}");
                    std::io::stdout().flush().ok();
                }
                None => break,
            }
        }
        _ = tokio::signal::ctrl_c() => { println!(); break; }
    }
}
```

Add `format_stream_status(e: &StatsStreamEvent) -> String` (similar to `format_status` but for the stream type — can share most formatting logic).

- [ ] `cargo check --workspace` + `mise run clippy`
- [ ] Manual test: start server, run `find-admin status --watch`, trigger an index, verify display updates

---

## Task 5: Final cleanup

- [ ] Run `cargo test --workspace`
- [ ] Run `mise run clippy`
- [ ] Update `CHANGELOG.md` `[Unreleased]`:
  - `GET /api/v1/stats/stream` — SSE endpoint, max 5 events/sec (configurable via `server.stats_stream_rate_hz`); `find-admin status --watch` now event-driven instead of polling
- [ ] Commit
