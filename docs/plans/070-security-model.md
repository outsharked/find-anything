# Security Model: User Authentication and Source-Level Access Control

## Overview

Add optional user authentication and role-based access control to limit which
sources a user can see. The model is designed for intranet/local-network use
and explicitly does not address threats from malicious actors, DDoS, or other
public-internet concerns.

Authentication is **opt-in at the server level**. A server with no auth
configuration behaves exactly as today — no login required, all sources
accessible to anyone who can reach the server. Authentication only kicks in
when the admin configures it, and even then, sources with no role restriction
remain visible without logging in.

---

## Goals

- Source-level access control: restrict visibility of a source to users holding
  a specific role
- Optional: if not configured, the server works exactly as today
- Username/password login for web UI and CLI read tools
- API keys (the existing token mechanism) narrowed to write-only (indexing)
- Session persistence for CLI tools so users don't re-authenticate constantly
- Clear, documented path for the "overlapping sources" partitioning pattern

## Non-Goals

- Per-file or per-directory ACLs within a source
- LDAP, OAuth, SAML, or any external identity provider
- Audit logging
- Refresh token rotation / OAuth-style token flows
- Protection against malicious actors (this is an intranet tool)
- Migration compatibility with existing deployments (pre-release; no migration needed)

---

## Design Decisions

### 1. Two credential types

**API keys** (write-only)
- The existing bearer token mechanism, narrowed in scope
- Can only call write endpoints: `POST /api/v1/bulk`, `POST /api/v1/upload`, `PATCH/HEAD /api/v1/upload/*`
- Cannot search, browse, or read any content
- Multiple named keys are possible (one per client machine or team)
- Each key is optionally scoped to a list of allowed source names (a client can
  only index sources it has been granted)
- Configured in `server.toml` under a new `[keys]` section; not stored in the
  user DB (keys are managed by the server admin, not end users)

**User sessions** (read-only by default, admin role for admin access)
- Username + password → session token
- All read endpoints (search, file, tree, context, sources list, etc.) require
  a valid session if the server has any auth configured
- Session token carries the user's role set
- Each request checks whether the user holds a role that permits access to the
  requested source
- Admin endpoints (`/api/v1/admin/*`) require the built-in `admin` role

### 2. Optional authentication

The server has a global `[auth] required = false` (default). When false:

- Sources with no `roles` configured are accessible without any authentication
- Sources with `roles` configured still require a valid session with a matching role
- The web UI shows a "Login" control but you can browse unrestricted sources without it

When `required = true`:

- All endpoints (including unrestricted sources) require a valid session token
- Unauthenticated requests to read endpoints receive HTTP 401
- This is the mode for deployments where even the existence of the index should
  be kept private

API keys are always required for write endpoints regardless of this setting.

### 3. Source-level RBAC via server.toml

Role requirements are defined server-side, not by the indexing client. The client
just submits content to a named source. The server admin decides who can read it.

```toml
[sources.hr-docs]
path = "/mnt/nas/hr"         # filesystem root for raw file serving
roles = ["hr", "admin"]      # only users with hr or admin role can see this

[sources.engineering]
path = "/mnt/nas/eng"
roles = ["engineering", "admin"]

[sources.public-docs]
# no roles = visible to all authenticated users (or unauthenticated if required=false)
```

### 4. The "overlapping sources" partitioning pattern

The recommended way to apply different access levels to different parts of a
filesystem is to define multiple client sources using the existing `include`
glob mechanism, each covering a non-overlapping subset of paths:

**client.toml on a client that indexes `/data`:**
```toml
[[sources]]
name = "hr-data"
path = "/data"
include = ["hr/**", "payroll/**"]

[[sources]]
name = "eng-data"
path = "/data"
include = ["engineering/**", "shared/**"]
```

**server.toml:**
```toml
[sources.hr-data]
roles = ["hr", "admin"]

[sources.eng-data]
roles = ["engineering", "admin"]
```

**Critical limitation — overlapping includes leak data:**

A file's effective access level is the **least restrictive** of all sources it
appears in. If the same file is covered by both a restricted and an unrestricted
source (or two sources with different roles), the stricter restriction is
ineffective — the file is reachable through whichever source is more permissive.

The server cannot detect or prevent this. Glob intersection is non-trivial, and
the server has no visibility into the client's include patterns at query time.
This is an **admin responsibility**.

Additionally, a file indexed in two sources produces duplicate search results
(same content appearing under two source labels). This is a pre-existing
behavior for overlapping sources and is not specific to the security model.

**Recommended practice:** Include patterns in different sources on the same
client should be **non-overlapping**. Use them to partition, not to alias.

### 5. User database

A new `auth.db` SQLite file in `data_dir`, separate from all source DBs.

Tables:
- `users`: `id`, `username` (unique), `password_hash` (argon2id), `created_at`,
  `last_login_at`, `enabled` (bool)
- `roles`: `id`, `name` (unique), `description`
- `user_roles`: `user_id`, `role_id`
- `sessions`: `token_hash`, `user_id`, `created_at`, `expires_at`, `last_seen_at`
- `api_keys`: `key_hash`, `name`, `allowed_sources` (JSON array or NULL for all),
  `created_at`, `last_used_at`

Password hashing uses `argon2` crate (argon2id variant). Session tokens are
random 32-byte values (hex-encoded), stored hashed in the DB.

Built-in `admin` role created automatically on DB initialization.

### 6. Session caching and performance

Session tokens are validated on every request. To avoid a DB lookup per request:
- Add `session_cache: Arc<Mutex<HashMap<String, CachedSession>>>` to `AppState`
- Cache entries store `(user_id, roles, expires_at)`, evicted when expired
- Cache is populated on first use of a token, invalidated on logout or expiry
- This makes the per-request auth check a single in-memory HashMap lookup in
  the common case — negligible performance impact

Source-access filtering is also in-memory: the user's role set (from cache) is
intersected with the source's required roles (from `AppState`, loaded at
startup from config). No DB query at search time.

Search performance impact: the search path already opens per-source SQLite DBs
in a loop. With access control, unauthorized source DBs are simply skipped. This
is cheaper than querying them — the filtering reduces work rather than adding it.

### 7. Bootstrap / first admin

If `auth.db` does not exist or contains no users, the server starts normally but
the web UI's login page shows a **first-run setup form** to create the initial
admin user. Once any admin-role user exists, this form disappears permanently.

Alternatively, an `find-admin user create --username X --password Y --role admin`
command can bootstrap users from the CLI without requiring the web UI (useful for
headless/automated deployments).

### 8. API key configuration

```toml
# server.toml
[[keys]]
name = "workstation-laptop"
key = "abc123..."                    # raw key; server hashes with blake3 before storing in auth.db
sources = ["engineering", "shared"]  # optional; omit to allow all sources

[[keys]]
name = "hr-scanner"
key = "def456..."
sources = ["hr-data"]
```

Keys grant access only to `POST /api/v1/bulk` and `POST /api/v1/upload*` for
the listed sources. A key for `sources = ["hr-data"]` that tries to submit a
bulk request with `source = "engineering"` gets HTTP 403.

If `sources` is omitted, the key can index any source (useful when a single
client indexes everything).

### 9. CLI credential storage

After a successful login, the session token is written to:
- Linux/macOS: `~/.local/share/find-anything/session`
- Windows: `%APPDATA%\find-anything\session`

File is created with mode `0600` (owner read/write only). Token TTL is
configurable in `server.toml` (default: 30 days). CLI tools read this file on
startup. If the token is absent, expired, or rejected by the server, the tool
prompts for username/password, calls `POST /api/v1/auth/login`, and writes the
new token.

`find-admin user login` (or a flag on existing tools) handles the interactive
login flow.

### 10. Share links (existing feature)

No change. Share links are already unauthenticated (they use a separate short
code). They remain useful for sharing specific files without granting source-wide
access. The link-creation endpoint (`POST /api/v1/links`) already requires
authentication, so only logged-in users can create links.

---

## Configuration Reference

### server.toml additions

```toml
[auth]
required = false           # if true, all endpoints require login (even for unrestricted sources)
session_ttl = "30d"        # how long user sessions last
                           # omit entire [auth] section to disable auth entirely

[[keys]]
name = "my-laptop"
key = "..."                # raw key — hashed with blake3 before storage in auth.db
sources = []               # empty or omit = all sources allowed

[sources.restricted-source]
path = "/mnt/restricted"
roles = ["restricted-role"]

[sources.open-source]
# no roles = accessible to all authenticated users (or unauthenticated if required=false)
```

### client.toml

No changes needed for indexing clients. The `token` field in `[server]` becomes
the API key. Behavior is identical from the client's perspective.

---

## Files Changed

### Server (`crates/server/`)

- `src/auth.rs` (new) — user DB management, session validation, password hashing,
  session cache; `check_index_key()` and `check_user_session()` extractors
- `src/routes/mod.rs` — replace `check_auth()` with two separate auth extractors;
  inject `UserContext` into all read handlers
- `src/routes/session.rs` — extend to handle username/password login
  (`POST /api/v1/auth/login`) in addition to the existing cookie-setting endpoint
- `src/routes/admin.rs` — require `admin` role instead of generic auth
- `src/routes/search.rs`, `file.rs`, `tree.rs`, `context.rs` etc. — filter
  sources by `UserContext.roles`; return 403 for unauthorized source access
- `src/routes/bulk.rs` (or equivalent) — validate API key scope (allowed sources)
- `src/lib.rs` — add `auth_db`, `session_cache` to `AppState`; load `[keys]`
  from config at startup
- `src/schema_auth.sql` (new) — SQL for `auth.db` schema

### Common (`crates/common/`)

- `src/config.rs` — add `AuthConfig`, `KeyConfig` structs; `roles` field on
  `ServerSourceConfig`
- `src/api.rs` — add `LoginRequest`/`LoginResponse` types

### Web UI (`web/`)

- `src/lib/api.ts` — add `login(username, password)` function; `getToken()`
  already works the same way (session token stored in localStorage)
- `src/routes/+layout.svelte` or equivalent — render login form vs. session state
- `src/lib/components/LoginForm.svelte` (new) — username/password form
- `src/routes/+page.svelte` — handle unauthenticated state for unrestricted
  sources; show login prompt for restricted sources the user tries to access

### CLI tools (`crates/client/`, `crates/admin/`)

- `find-admin`: add `user` subcommand group (`create`, `list`, `delete`,
  `set-password`, `assign-role`) and `login` subcommand
- `find-scan` / `find-watch`: no functional changes; configuration field rename
  from "token" to "key" is optional (keep backward-compat alias)
- Session token file read/write logic (shared helper, probably in `find-common`)

---

## Search Behavior with Access Control

### `GET /api/v1/search`

Current: queries all source DBs, aggregates results.
New: queries only source DBs where `user.roles ∩ source.roles ≠ ∅`, or where
the source has no role requirement. Unauthorized sources are silently excluded
(they don't appear in results). HTTP 200 with potentially fewer results — not
a 403, because the query itself is valid; the user just has a smaller universe.

### `GET /api/v1/sources`

Returns only sources the user is authorized to see.

### `GET /api/v1/tree`, `GET /api/v1/file`, `GET /api/v1/context`

Take a `?source=X` parameter. If the user lacks access to source X, return
HTTP 401 (unauthenticated) or HTTP 403 (authenticated but insufficient role),
consistent with the principle that 401 prompts login while 403 signals a
privilege boundary.

### Unauthenticated requests (when `required = false`)

Treated as a virtual user with no roles. Sees only sources with no `roles`
configured. Restricted sources are invisible in search results; a direct request
to a restricted source's tree/file returns HTTP 401 (not 403, to prompt login).

---

## Testing Strategy

- Unit tests for `auth.rs`: password hashing/verification, session creation,
  expiry, cache eviction
- Integration tests (extending existing `tests/integration/`):
  - Server with no auth configured — existing tests pass unchanged
  - Server with `required = false` — unauthenticated client sees only
    unrestricted sources; authenticated client sees its role's sources
  - Server with `required = true` — unauthenticated client gets 401
  - API key scope enforcement — key limited to `["source-a"]` cannot index
    `source-b`
  - Overlapping sources test: same file indexed in two sources; verify it
    appears in results for users who can see either source

---

## Decisions

1. **Key storage** — Raw keys are written in `server.toml` (admin-managed
   config) but are hashed with blake3 and stored in `auth.db`. The raw key
   is never persisted to the DB; only the hash is stored. On each request the
   presented key is hashed with blake3 and compared against stored hashes.
   Blake3 is used rather than argon2id because API keys are high-entropy random
   strings (not human-chosen passwords), so a fast hash is appropriate and
   avoids per-request argon2 cost when iterating multiple keys. Passwords
   continue to use argon2id.

2. **Search result filtering** — Restricted sources are silently omitted from
   search results. No indication that they exist is given to an unauthenticated
   or unauthorized user.

3. **Unauthenticated direct access to restricted source** — Returns HTTP 401
   to signal "login may help", not 403 ("you're definitely not allowed").

## Open Questions

1. **Role management UI** — First cut: admin manages roles via CLI only
   (`find-admin user assign-role`). A settings page for role/user management
   can come later.

3. **Source roles in server.toml vs. separate file** — `server.toml` is fine
   for most deployments. If the number of sources grows very large, a separate
   `roles.toml` might be cleaner, but this is premature.

4. **Session invalidation on password change** — Should changing a user's
   password invalidate existing sessions? Simplest: yes, clear all sessions for
   that user from the DB and cache on password change.
