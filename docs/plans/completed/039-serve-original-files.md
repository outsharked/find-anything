# Serve Original Files (plan 039)

## Overview

Files indexed by find-anything live on the client's filesystem. The server currently
only serves extracted text content (stored in ZIP archives). Users want to view the
**original file** — especially for images (which benefit from inline rendering) and
for any file where seeing the original is useful. This requires:

1. The server to know the filesystem path to each source's root directory.
2. A new authenticated endpoint to stream original files.
3. Cookie-based auth so `<img src="...">` tags work natively (the current
   header-only auth model cannot authenticate browser-initiated resource requests).
4. UI integration in `FileViewer.svelte` with sensible defaults per file type.

---

## Auth Change: Cookie-Based Session

**Problem:** `check_auth()` only inspects the `Authorization: Bearer` header.
Browser-native requests (`<img src>`, download links, future video, etc.) never
include custom headers, so they always get 401.

**Solution:** Add a `POST /api/v1/auth/session` endpoint. When the user enters the
token, the frontend calls this endpoint, which validates the token and sets an
`HttpOnly; SameSite=Strict; Path=/` session cookie named `find_session`. The cookie
value is the token itself (simple shared-secret model — no separate session layer).

`check_auth()` is updated to accept **either** the Authorization header **or** the
`find_session` cookie. Existing API clients that use headers continue to work.

**localStorage is the source of truth and is unchanged.** The token continues to be
stored there so it survives browser restarts. The cookie is a derived, ephemeral
artifact:

- On every page load, if localStorage has a token, the frontend calls
  `/api/v1/auth/session` to (re-)create the cookie.
- When the user enters/changes the token, `saveToken()` writes it to localStorage
  *and* calls the session endpoint to set the cookie immediately.
- Logging out (clearing the token) also explicitly clears the cookie client-side.

---

## Config Change

Add `ServerSourceConfig` to `ServerAppConfig` in `crates/common/src/config.rs`:

```rust
#[serde(default)]
pub sources: std::collections::HashMap<String, ServerSourceConfig>,
```

New struct:
```rust
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServerSourceConfig {
    pub path: Option<String>,
}
```

---

## New Endpoint: `GET /api/v1/raw`

**Route:** `GET /api/v1/raw?source=<name>&path=<relative_path>`

- `source` — source name (must be configured with a `path`; 404 if not configured)
- `path` — relative file path within the source root (must not contain `..` or
  start with `/`; 400 if invalid); archive member paths (`::` separator) are
  rejected with 400 — only real files can be served

**Auth:** via `check_auth()` (either header or cookie).

**Implementation:**
1. Validate `path` — reject `..` components, leading `/`, or `::` substring.
2. Resolve `full_path = source_root.join(path)`.
3. Canonicalize and confirm it still starts with the canonicalized `source_root`
   (path traversal guard).
4. Stream the file using `tokio::fs::File` with appropriate `Content-Type`
   (inferred from extension via the `mime_guess` crate) and `Content-Disposition: inline`.

---

## Auth Session Endpoint

`POST /api/v1/auth/session` validates the token and sets `find_session` cookie.

`check_auth()` updated to accept either the Authorization header or the cookie.

---

## Files Changed

| File | Change |
|------|--------|
| `crates/common/src/config.rs` | Add `ServerSourceConfig` struct; add `sources` field to `ServerAppConfig` |
| `examples/server.toml` | Add commented-out `[sources.NAME]` example |
| `crates/server/src/routes/session.rs` | New — `POST /api/v1/auth/session` |
| `crates/server/src/routes/raw.rs` | New — `GET /api/v1/raw` with path traversal guard |
| `crates/server/src/routes/mod.rs` | Update `check_auth()` to accept cookie; add module imports |
| `crates/server/src/main.rs` | Register `/api/v1/auth/session` and `/api/v1/raw` routes |
| `web/src/lib/api.ts` | Add `activateSession()` |
| `web/src/routes/+page.svelte` | Call `activateSession()` on token save and on page load |
| `web/src/lib/FileViewer.svelte` | Add `showOriginal` state, toolbar button, original panel, CSS |

---

## Testing

1. **Cookie set on login**: Enter token in UI → DevTools → Application → Cookies →
   `find_session` cookie present with `HttpOnly` flag.
2. **Cookie persists**: Reload page — cookie still present; no need to re-enter token.
3. **Backward compat**: `curl -H "Authorization: Bearer <token>" ...` still works.
4. **Raw endpoint**: `curl` with auth header → file content returned with correct `Content-Type`.
5. **Path traversal guard**: `?path=../../etc/passwd` → 400 Bad Request.
6. **Archive member guard**: `?path=foo.zip::bar.txt` → 400 Bad Request.
7. **No source path configured**: Raw endpoint returns 404.
8. **Image display**: Search for an image → open file detail → inline image shown by default.
9. **Text file**: Open detail → extracted text shown by default; "Original" button switches to iframe.
10. **PDF**: "Original" renders as a link that opens in new tab.
