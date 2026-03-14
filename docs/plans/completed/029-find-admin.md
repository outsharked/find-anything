# Plan: find-admin Tool

## Context

The existing `find-config` binary only prints effective client config. The roadmap
calls for a unified `find-admin` binary covering config inspection, server stats,
source listing, connectivity checks, and inbox management. Admin operations go over
HTTP using the same bearer token as all other client tools — no extra auth layer
for now. `find-config` is removed and replaced.

---

## Architecture Overview

```
find-admin (new binary, crates/client/src/admin_main.rs)
    --config PATH    (optional, same default as all client tools)
    --json           (emit JSON instead of human-readable text)

Subcommands:
    config          → print effective client config (TOML or JSON)
    stats           → per-source stats via GET /api/v1/stats
    sources         → list sources via GET /api/v1/sources
    check           → connectivity + auth health check
    inbox           → inbox status via GET /api/v1/admin/inbox (NEW endpoint)
    inbox-clear     → DELETE /api/v1/admin/inbox?target=pending|failed|all
                      --failed flag: target failed/ instead of pending
                      --all flag: target both
                      --yes flag: skip confirmation prompt
    inbox-retry     → POST /api/v1/admin/inbox/retry  (NEW endpoint)
                      --yes flag: skip confirmation prompt
```

---

## Part 1 — New Server Endpoints

### New file: `crates/server/src/routes/admin.rs`

Three handlers. All require bearer-token auth. All operate on `state.data_dir/inbox/`.
Use `spawn_blocking` for filesystem I/O (consistent with other route handlers).

#### `GET /api/v1/admin/inbox`
Lists pending and failed inbox files with name, size, and age.

```rust
// New types in crates/common/src/api.rs:
pub struct InboxItem { pub filename: String, pub size_bytes: u64, pub age_secs: u64 }
pub struct InboxStatusResponse { pub pending: Vec<InboxItem>, pub failed: Vec<InboxItem> }
```

Implementation: `read_dir(inbox_dir)` (excluding `failed/`) and `read_dir(inbox_dir/failed/)`,
collect `.gz` files, read `metadata()` for size and `modified()` for age.

#### `DELETE /api/v1/admin/inbox?target=pending|failed|all`
Deletes inbox `.gz` files. Default target: `pending`.

```rust
pub struct InboxDeleteResponse { pub deleted: usize }
```

Implementation: iterate appropriate dir(s), delete each `.gz`, return count.

#### `POST /api/v1/admin/inbox/retry`
Moves all `.gz` files from `inbox/failed/` to `inbox/`.

```rust
pub struct InboxRetryResponse { pub retried: usize }
```

Implementation: `read_dir(failed_dir)`, `rename` each `.gz` to `inbox_dir/<filename>`.

### Changes to `crates/server/src/routes/mod.rs`
Add `mod admin; pub use admin::{inbox_status, inbox_clear, inbox_retry};`

### Changes to `crates/server/src/main.rs`
```rust
.route("/api/v1/admin/inbox",       get(routes::inbox_status))
.route("/api/v1/admin/inbox",       delete(routes::inbox_clear))
.route("/api/v1/admin/inbox/retry", post(routes::inbox_retry))
```

---

## Part 2 — New Common API Types

### Changes to `crates/common/src/api.rs`
Add: `InboxItem`, `InboxStatusResponse`, `InboxDeleteResponse`, `InboxRetryResponse`
(all derive `Serialize`, `Deserialize`, `Debug`).

---

## Part 3 — Extend ApiClient

### Changes to `crates/client/src/api.rs`
Add six new async methods following the exact existing pattern
(`.bearer_auth(&self.token)`, `.error_for_status()?`, `.json::<T>()`):

```rust
pub async fn get_stats(&self) -> Result<StatsResponse>          // GET /api/v1/stats
pub async fn get_sources(&self) -> Result<Vec<SourceInfo>>      // GET /api/v1/sources
pub async fn get_settings(&self) -> Result<AppSettingsResponse> // GET /api/v1/settings
pub async fn inbox_status(&self) -> Result<InboxStatusResponse> // GET /api/v1/admin/inbox
pub async fn inbox_clear(&self, target: &str) -> Result<InboxDeleteResponse>
    // DELETE /api/v1/admin/inbox?target=<target>
pub async fn inbox_retry(&self) -> Result<InboxRetryResponse>   // POST /api/v1/admin/inbox/retry
```

---

## Part 4 — New `find-admin` Binary

### New file: `crates/client/src/admin_main.rs`

#### CLI structure (clap derive)
```rust
#[derive(Parser)]
#[command(name = "find-admin", about = "Administrative utilities for find-anything")]
struct Args {
    #[arg(long, global = true)]
    config: Option<String>,
    #[arg(long, global = true, help = "Output raw JSON")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Config,
    Stats,
    Sources,
    Check,
    Inbox,
    InboxClear { #[arg(long)] failed: bool, #[arg(long)] all: bool, #[arg(long)] yes: bool },
    InboxRetry { #[arg(long)] yes: bool },
}
```

#### Subcommand behaviour

**`config`**: No server call. Load client config, print as `toml::to_string_pretty`
(or `serde_json::to_string_pretty` with `--json`).

**`stats`**: Call `client.get_stats()`. Human output: per-source table
(files, total size, last-scan age). JSON: pass through.

**`sources`**: Call `client.get_sources()`. Human output: numbered list with name +
base_url (or "none"). JSON: pass through.

**`check`**: Call `get_settings()` + `get_sources()`. Print ✓/✗ per check with
`colored` (green/red). Exit code 1 if any check fails:
```
✓  Server reachable at http://localhost:8765
✓  Authenticated (token accepted)
✓  Server version: 0.2.6
✓  3 source(s) indexed
```

**`inbox`**: Call `client.inbox_status()`. Print pending + failed file lists with
sizes and ages. JSON: pass through.

**`inbox-clear`**:
- Determine target: `--all` → "all", `--failed` → "failed", else "pending"
- Unless `--yes`, call `inbox_status()` first and print
  "Clear N pending file(s)? [y/N]" — read stdin; abort if not 'y'/'Y'
- Call `client.inbox_clear(target)`; print "Deleted N file(s)."

**`inbox-retry`**:
- Unless `--yes`, call `inbox_status()` first and print
  "Retry N failed file(s)? [y/N]"
- Call `client.inbox_retry()`; print "Retried N file(s)."

#### Config loading (same as all other binaries)
```rust
let config_path = args.config.unwrap_or_else(default_config_path);
let config_str = fs::read_to_string(&config_path)
    .with_context(|| format!("reading config: {config_path}"))?;
let config = parse_client_config(&config_str)?;
let client = ApiClient::new(&config.server.url, &config.server.token);
```

Commands that don't need a server (`config`) can skip building `ApiClient`.

---

## Part 5 — Remove `find-config`

- **Delete** `crates/client/src/config_main.rs`
- Remove `[[bin]] name = "find-config"` from `crates/client/Cargo.toml`
- Add `[[bin]] name = "find-admin"` with `path = "src/admin_main.rs"`
- Update `install.sh`: replace `find-config` with `find-admin` in binary list;
  update any mention in install summary messages
- Update `.github/workflows/release.yml`: add `find-admin` to artifact list;
  remove `find-config`
- Search for any other references: `docs/`, `README.md`, systemd unit files

---

## Part 6 — ROADMAP & CHANGELOG

- ROADMAP: move `find-admin` from Near-term to ✅ Recently Completed with version tag
- ROADMAP: add security note under the entry: admin endpoints use the same bearer
  token; RBAC planned for a future release
- CHANGELOG `[Unreleased]`: add `find-admin` binary; note `find-config` removal

---

## Files Changed Summary

| File | Change |
|------|--------|
| `crates/common/src/api.rs` | Add inbox response types |
| `crates/server/src/routes/admin.rs` | **New** — three inbox admin handlers |
| `crates/server/src/routes/mod.rs` | Re-export admin handlers |
| `crates/server/src/main.rs` | Register three new routes |
| `crates/client/src/api.rs` | Add 6 new async methods |
| `crates/client/src/admin_main.rs` | **New** — find-admin binary |
| `crates/client/src/config_main.rs` | **Delete** |
| `crates/client/Cargo.toml` | Add `find-admin` bin; remove `find-config` bin |
| `.github/workflows/release.yml` | Add `find-admin`; remove `find-config` |
| `install.sh` | Replace `find-config` with `find-admin` |
| `ROADMAP.md` | Mark complete; add security note |
| `CHANGELOG.md` | Add entry |
