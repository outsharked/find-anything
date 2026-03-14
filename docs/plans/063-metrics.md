# Metrics and Observability

## Overview

find-anything already collects useful timing data — `extract_ms` per file in
the `files` table, debug-level timing macros in the inbox worker and archive
batch, and the `GET /api/v1/stats` snapshot endpoint (queue depths, file
counts, storage sizes). What's missing is a way to feed this data into an
external time-series system (Prometheus, Grafana Mimir, InfluxDB, Datadog,
VictoriaMetrics) without coupling to any one of them.

The goal: instrument the code once using a generic facade, configure any
backend via `server.toml`, and let users wire it to whatever aggregation stack
they already run.

---

## Key Design Decision: `metrics` crate as facade

The [`metrics`](https://crates.io/crates/metrics) crate is the Rust equivalent
of `log`/`tracing` for metrics — a thin macro facade:

```rust
metrics::counter!("files_indexed_total", 1, "source" => source_name);
metrics::histogram!("extraction_duration_ms", duration_ms, "kind" => kind);
metrics::gauge!("inbox_queue_depth", depth as f64);
```

When no backend is installed, all calls are no-ops (zero overhead). Backends
are plugged in at startup. This means:

- Instrumentation code is written once and never changes when the backend changes.
- Adding a new backend (e.g. StatsD) in future requires zero changes to the
  call sites.
- Users who don't configure metrics pay nothing.

---

## Backends

### Backend 1: Prometheus `/metrics` endpoint (pull model)

`metrics-exporter-prometheus` exposes an HTTP endpoint in the standard
Prometheus text format. Any scraper can consume it: Prometheus, Grafana Agent,
OpenTelemetry Collector, VictoriaMetrics, `curl`.

This is the **recommended primary backend** because:
- No push destination required — the scraper comes to us
- Works with every modern observability stack (all of them speak Prometheus)
- For Grafana LGTM: the Grafana Agent scrapes `/metrics` and forwards to Mimir

Configuration in `server.toml`:
```toml
[metrics]
# Expose a Prometheus /metrics endpoint.
prometheus = true
# Defaults to the same port as the main server.
# Set to a different port to isolate the metrics endpoint from auth.
# prometheus_port = 9090
```

`/metrics` on the same port is simplest for users without a multi-port setup.
Optional: same bearer-token auth as the rest of the API, or unauthenticated
(standard Prometheus practice, fine on a LAN).

### Backend 2: Push to a remote URL (push model)

At a configurable interval, POST a JSON payload to a user-supplied URL. This
covers:
- **Grafana LGTM Mimir** push endpoint (`/api/v1/push`, Prometheus remote-write)
- **InfluxDB** line protocol endpoint
- **Custom scripts** — any HTTP receiver
- Webhooks into home automation, alerting, etc.

The payload format is configurable:
- `"prometheus_remote_write"` — Prometheus remote-write protobuf (works with
  Mimir, Cortex, Thanos, VictoriaMetrics)
- `"json"` — simple JSON array of `{metric, labels, value, timestamp}` objects
  (works with anything)

Configuration:
```toml
[metrics]
push_url = "http://mimir:9009/api/v1/push"
push_format = "prometheus_remote_write"  # or "json"
push_interval_secs = 15
```

### Non-goal: OpenTelemetry traces

OTLP traces are a separate concern (request tracing across spans). If OTLP
metrics become the obvious standard in 12–24 months, the `metrics` crate can
grow an OTLP backend without changing any call sites.

---

## What to Instrument

### `find-server` (counters, gauges, histograms)

| Metric | Type | Labels | Source |
|---|---|---|---|
| `files_indexed_total` | counter | `source`, `kind` | pipeline.rs on upsert |
| `files_deleted_total` | counter | `source` | pipeline.rs on delete |
| `extraction_duration_ms` | histogram | `kind` | `extract_ms` from bulk request |
| `inbox_queue_depth` | gauge | — | `StatsResponse.inbox_pending` |
| `archive_queue_depth` | gauge | — | `StatsResponse.archive_queue` |
| `inbox_processing_duration_ms` | histogram | — | existing timing macro in worker |
| `archive_write_duration_ms` | histogram | — | existing timing macro in archive_batch |
| `compaction_orphaned_bytes` | gauge | — | compaction scanner |
| `compaction_duration_ms` | histogram | — | compaction scanner |
| `http_requests_total` | counter | `method`, `path`, `status` | `TraceLayer` / tower middleware |
| `http_request_duration_ms` | histogram | `method`, `path` | tower middleware |
| `db_size_bytes` | gauge | `source` | `StatsResponse.db_size_bytes` |
| `archive_size_bytes` | gauge | — | `StatsResponse.archive_size_bytes` |

### `find-scan` (reported per-run, pushed to server)

`find-scan` is short-lived and doesn't hold a process open long enough for a
pull-based scrape to be useful. Options:

**Option A:** `find-scan` pushes aggregate stats to `find-server` at end of
scan via a new `POST /api/v1/metrics/scan` endpoint; server aggregates and
exposes via `/metrics`. Keeps all metric emission in one place.

**Option B:** `find-scan` emits metrics directly to the push URL (if
configured) at end of run, independently of `find-server`. Simpler but
requires duplicating the backend config.

**Recommendation:** Option A. `find-server` becomes the single metrics
aggregation point for the whole system.

Scan metrics pushed to server:

| Metric | Type | Labels |
|---|---|---|
| `scan_files_visited_total` | counter | `source` |
| `scan_files_new_total` | counter | `source` |
| `scan_files_modified_total` | counter | `source` |
| `scan_files_unchanged_total` | counter | `source` |
| `scan_files_excluded_total` | counter | `source` |
| `scan_errors_total` | counter | `source` |
| `scan_duration_ms` | histogram | `source` |

---

## Architecture

```
find-scan ──POST /api/v1/metrics/scan──► find-server
                                              │
                              ┌───────────────┼────────────────┐
                              ▼               ▼                ▼
                       in-process       Prometheus        push to
                       metrics store    /metrics           remote URL
                       (metrics crate)  endpoint           (interval)
```

`find-server` runs a `metrics-exporter-prometheus` recorder from startup.
All `metrics::counter!` / `metrics::gauge!` / `metrics::histogram!` calls
update it in-memory. The `/metrics` endpoint reads and renders it on demand.
The push task (if configured) reads the same store at each interval.

---

## Implementation

### 1. New `[metrics]` config section (`crates/common/src/config.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MetricsConfig {
    /// Expose a Prometheus /metrics endpoint on the main server port.
    #[serde(default)]
    pub prometheus: bool,

    /// Optional separate port for /metrics (avoids mixing with auth).
    pub prometheus_port: Option<u16>,

    /// Push metrics to this URL at `push_interval_secs`.
    pub push_url: Option<String>,

    /// Format for push: "json" or "prometheus_remote_write".
    #[serde(default = "default_push_format")]
    pub push_format: String,  // "json"

    /// Push interval in seconds. Default: 15.
    #[serde(default = "default_push_interval")]
    pub push_interval_secs: u64,
}
```

### 2. `metrics` crate instrumentation

Add to `find-server`'s `Cargo.toml`:
```toml
metrics = { workspace = true }
metrics-exporter-prometheus = "0.15"
```

Add to workspace `Cargo.toml`:
```toml
metrics = "0.23"
```

`find-common` and `find-client` get just `metrics` (the facade, not the
exporter) so they can emit without pulling in the Prometheus exporter.

### 3. Startup (`crates/server/src/lib.rs`)

```rust
if config.metrics.prometheus {
    let builder = metrics_exporter_prometheus::PrometheusBuilder::new();
    let handle = builder.install_recorder()?;
    // store handle in AppState for /metrics handler
}
```

### 4. `/metrics` route

```rust
// No auth (standard Prometheus practice) or optional token check.
async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    match &state.prometheus_handle {
        Some(h) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
            h.render(),
        ).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
```

### 5. Instrumentation call sites

Mostly drop-in additions at existing timing/counting points:

```rust
// pipeline.rs — after upsert
metrics::counter!("files_indexed_total", 1, "source" => source, "kind" => kind);

// worker/mod.rs — existing time_step! macro gets a histogram emit
metrics::histogram!("inbox_processing_duration_ms", elapsed_ms);

// routes — add tower-http metrics layer
```

### 6. New `POST /api/v1/metrics/scan` endpoint

Small JSON body carrying the scan counters. Server calls `metrics::counter!`
etc. to roll them into the global recorder. No persistence needed — on restart
counts reset (standard Prometheus behavior for counters).

### 7. Push task

A background `tokio::spawn` loop wakes every `push_interval_secs` and POSTs
the current metric snapshot to `push_url`. For `"json"` format this is simple.
For `"prometheus_remote_write"` the `prometheus` crate's `proto` module encodes
the protobuf.

---

## Grafana LGTM Quick-Start (for docs)

```toml
# server.toml
[metrics]
prometheus = true   # expose /metrics on same port

# Then in grafana-agent.yaml:
# scrape_configs:
#   - job_name: find-anything
#     static_configs:
#       - targets: ["localhost:7842"]
```

Or push directly to Mimir:
```toml
[metrics]
push_url = "http://mimir:9009/api/v1/push"
push_format = "prometheus_remote_write"
push_interval_secs = 15
```

---

## Files Changed

- `crates/common/src/config.rs` — add `MetricsConfig`
- `crates/common/src/api.rs` — add `ScanMetricsRequest` (for scan push endpoint)
- `crates/common/Cargo.toml` — add `metrics` facade
- `crates/server/Cargo.toml` — add `metrics`, `metrics-exporter-prometheus`
- `crates/server/src/lib.rs` — recorder setup, push task spawn
- `crates/server/src/routes/` — add `/metrics` and `/api/v1/metrics/scan` handlers
- `crates/server/src/worker/pipeline.rs` — counter increments
- `crates/server/src/worker/mod.rs` — histogram in existing timing macro
- `crates/server/src/worker/compaction.rs` — gauge + histogram
- `crates/client/Cargo.toml` — add `metrics` facade
- `crates/client/src/scan.rs` — post scan metrics at end of run
- `examples/server.toml` — document `[metrics]` section
- `install.sh` / `find-anything.iss` — add commented `[metrics]` block

## Open Questions

- **Auth on `/metrics`**: unauthenticated (standard Prometheus practice) or
  same token as API? Unauthenticated is simpler and consistent with how
  Prometheus is normally deployed behind a firewall.
- **Counter reset on restart**: Prometheus counters naturally reset on process
  restart. This is expected behaviour. Alert rules should use `rate()` not raw
  values.
- **`find-watch` metrics**: watch events (files created/modified/deleted
  detected) could also be metered. Left for a follow-up.
- **Cardinality**: labelling `extraction_duration_ms` by `kind` is fine (~10
  kinds). Do NOT label by `path` or `source` on histograms — cardinality
  explosion risk. `source` on counters is fine (small, bounded set).

## Breaking Changes

None. `[metrics]` section is entirely opt-in. Existing behaviour is unchanged
when the section is absent.
