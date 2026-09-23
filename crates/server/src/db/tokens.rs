//! Access-token and invite storage (plan 094).
//!
//! `data_dir/tokens.db` holds named, scoped, optionally-expiring access tokens
//! and short-lived single-use invites that are redeemed for tokens. Only
//! BLAKE3 hashes of tokens/invite codes are stored (both are high-entropy
//! random values, so a fast hash is sufficient); the plaintext is shown to the
//! caller exactly once.
//!
//! The root admin token from `server.toml` is *not* stored here — it is
//! config-defined and never revocable through the API.
//!
//! This is not a source DB, so the "all writes go through the inbox worker"
//! invariant does not apply.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use find_common::api::{InviteInfo, RedeemResponse, Scope, TokenInfo};

/// Default lifetime of an invite when the caller does not specify one.
pub const DEFAULT_INVITE_TTL_SECS: u64 = 15 * 60;

/// How long a resolved token stays in the in-memory cache before it is
/// re-checked against SQLite.
const CACHE_TTL: Duration = Duration::from_secs(30);

/// `last_used_at` is only rewritten when it is at least this stale.
const LAST_USED_GRANULARITY_SECS: i64 = 60;

/// Crockford base32: no `I`, `L`, `O` or `U`.
const CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_LEN: usize = 8;

/// Prefix on every generated token so leaked tokens are easy to grep for.
pub const TOKEN_PREFIX: &str = "fa_";

/// The authenticated caller of a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub name: String,
    pub scope: Scope,
}

// ── Schema / hashing / generation ──────────────────────────────────────────────

pub fn open_tokens_db(data_dir: &Path) -> Result<Connection> {
    let db_path = data_dir.join("tokens.db");
    let conn = Connection::open(&db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tokens (
            id           INTEGER PRIMARY KEY,
            name         TEXT NOT NULL UNIQUE,
            token_hash   TEXT NOT NULL UNIQUE,
            scope        TEXT NOT NULL,
            created_at   INTEGER NOT NULL,
            expires_at   INTEGER,
            last_used_at INTEGER
        );
        CREATE TABLE IF NOT EXISTS invites (
            id             INTEGER PRIMARY KEY,
            code_hash      TEXT NOT NULL UNIQUE,
            name           TEXT NOT NULL,
            scope          TEXT NOT NULL,
            token_ttl_secs INTEGER,
            created_at     INTEGER NOT NULL,
            expires_at     INTEGER NOT NULL
        );",
    )
    .context("creating tokens/invites tables")
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// BLAKE3 hex digest of a token or normalised invite code.
pub fn hash_secret(secret: &str) -> String {
    blake3::hash(secret.as_bytes()).to_hex().to_string()
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("OS random source failed: {e}"))?;
    Ok(buf)
}

/// A fresh 256-bit token: `fa_` + 64 hex chars.
pub fn generate_token() -> Result<String> {
    let bytes = random_bytes::<32>()?;
    let mut s = String::with_capacity(TOKEN_PREFIX.len() + 64);
    s.push_str(TOKEN_PREFIX);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    Ok(s)
}

/// A fresh invite code in canonical form (8 Crockford base32 chars, no dash).
fn generate_code() -> Result<String> {
    let bytes = random_bytes::<CODE_LEN>()?;
    // 256 is a multiple of 32, so masking is unbiased.
    Ok(bytes
        .iter()
        .map(|b| CODE_ALPHABET[(*b & 31) as usize] as char)
        .collect())
}

/// Human-facing form of a canonical code: `XXXX-XXXX`.
pub fn format_code(canonical: &str) -> String {
    let (a, b) = canonical.split_at(canonical.len() / 2);
    format!("{a}-{b}")
}

/// Canonicalise user input: uppercase, drop dashes/whitespace, apply the
/// Crockford confusable mapping (`I`/`L` → `1`, `O` → `0`).
pub fn normalize_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect()
}

fn parse_scope(s: &str) -> Result<Scope> {
    s.parse::<Scope>().map_err(|e| anyhow::anyhow!("corrupt scope in tokens.db: {e}"))
}

// ── Invites ────────────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub enum CreateInviteError {
    /// A token or a pending invite already uses this name.
    NameTaken,
    /// Name is empty or contains control characters.
    InvalidName,
}

/// Create an invite. Returns `(display_code, expires_at)`.
pub fn create_invite(
    conn: &Connection,
    name: &str,
    scope: Scope,
    ttl_secs: u64,
    token_ttl_secs: Option<u64>,
    now: i64,
) -> Result<std::result::Result<(String, i64), CreateInviteError>> {
    let name = name.trim();
    if name.is_empty() || name.chars().any(|c| c.is_control()) {
        return Ok(Err(CreateInviteError::InvalidName));
    }
    conn.execute("DELETE FROM invites WHERE expires_at <= ?1", params![now])?;
    let taken: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM tokens WHERE name = ?1)
                 OR EXISTS(SELECT 1 FROM invites WHERE name = ?1)",
            params![name],
            |r| r.get(0),
        )
        .context("checking name uniqueness")?;
    if taken {
        return Ok(Err(CreateInviteError::NameTaken));
    }

    let expires_at = now.saturating_add(ttl_secs.min(i64::MAX as u64 / 2) as i64);
    let ttl_col = token_ttl_secs.map(|t| t.min(i64::MAX as u64 / 2) as i64);
    let code = generate_code()?;
    conn.execute(
        "INSERT INTO invites (code_hash, name, scope, token_ttl_secs, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![hash_secret(&code), name, scope.as_str(), ttl_col, now, expires_at],
    )
    .context("inserting invite")?;
    Ok(Ok((format_code(&code), expires_at)))
}

pub fn list_invites(conn: &Connection, now: i64) -> Result<Vec<InviteInfo>> {
    conn.execute("DELETE FROM invites WHERE expires_at <= ?1", params![now])?;
    let mut stmt = conn.prepare(
        "SELECT id, name, scope, created_at, expires_at FROM invites ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?, r.get::<_, i64>(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, name, scope, created_at, expires_at) = row?;
        out.push(InviteInfo { id, name, scope: parse_scope(&scope)?, created_at, expires_at });
    }
    Ok(out)
}

/// Delete a pending invite. Returns whether one was removed.
pub fn revoke_invite(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn.execute("DELETE FROM invites WHERE id = ?1", params![id])? > 0)
}

#[derive(Debug)]
pub enum RedeemOutcome {
    Redeemed(RedeemResponse),
    /// Unknown, expired or already-used code.
    Invalid,
    /// The token name was taken between invite creation and redemption. The
    /// invite is left intact.
    NameTaken,
}

/// Atomically consume an invite and mint its token.
pub fn redeem_invite(conn: &mut Connection, code: &str, now: i64) -> Result<RedeemOutcome> {
    let code_hash = hash_secret(&normalize_code(code));
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

    tx.execute("DELETE FROM invites WHERE expires_at <= ?1", params![now])?;
    // DELETE ... RETURNING makes "single use" atomic with the lookup.
    let row: Option<(String, String, Option<i64>)> = tx
        .query_row(
            "DELETE FROM invites WHERE code_hash = ?1
             RETURNING name, scope, token_ttl_secs",
            params![code_hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((name, scope, token_ttl)) = row else {
        return Ok(RedeemOutcome::Invalid);
    };
    let scope = parse_scope(&scope)?;

    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM tokens WHERE name = ?1)",
        params![name],
        |r| r.get(0),
    )?;
    if taken {
        // Dropping `tx` rolls back the invite deletion.
        return Ok(RedeemOutcome::NameTaken);
    }

    let token = generate_token()?;
    let expires_at = token_ttl.map(|t| now.saturating_add(t));
    tx.execute(
        "INSERT INTO tokens (name, token_hash, scope, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![name, hash_secret(&token), scope.as_str(), now, expires_at],
    )
    .context("inserting token")?;
    tx.commit()?;

    Ok(RedeemOutcome::Redeemed(RedeemResponse { token, name, scope, expires_at }))
}

// ── Tokens ─────────────────────────────────────────────────────────────────────

pub fn list_tokens(conn: &Connection) -> Result<Vec<TokenInfo>> {
    let mut stmt = conn.prepare(
        "SELECT name, scope, created_at, expires_at, last_used_at FROM tokens ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?,
            r.get::<_, Option<i64>>(3)?, r.get::<_, Option<i64>>(4)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (name, scope, created_at, expires_at, last_used_at) = row?;
        out.push(TokenInfo { name, scope: parse_scope(&scope)?, created_at, expires_at, last_used_at });
    }
    Ok(out)
}

/// Delete a token by name. Returns whether one was removed.
pub fn revoke_token(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM tokens WHERE name = ?1", params![name])? > 0)
}

/// Raw `tokens` row: `(id, name, scope, expires_at, last_used_at)`.
type RawTokenRow = (i64, String, String, Option<i64>, Option<i64>);

struct TokenRow {
    name: String,
    scope: Scope,
    expires_at: Option<i64>,
}

/// Look up a token by the hash of its value. Returns `None` if unknown or
/// expired. Refreshes `last_used_at` when it is stale.
fn lookup_token(conn: &Connection, token_hash: &str, now: i64) -> Result<Option<TokenRow>> {
    let row: Option<RawTokenRow> = conn
        .query_row(
            "SELECT id, name, scope, expires_at, last_used_at FROM tokens WHERE token_hash = ?1",
            params![token_hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((id, name, scope, expires_at, last_used)) = row else { return Ok(None) };
    if expires_at.is_some_and(|e| e <= now) {
        return Ok(None);
    }
    if last_used.is_none_or(|t| now - t >= LAST_USED_GRANULARITY_SECS) {
        conn.execute("UPDATE tokens SET last_used_at = ?1 WHERE id = ?2", params![now, id])?;
    }
    Ok(Some(TokenRow { name, scope: parse_scope(&scope)?, expires_at }))
}

// ── TokenStore: shared connection + resolve cache ──────────────────────────────

struct CacheEntry {
    principal: Principal,
    expires_at: Option<i64>,
    cached_at: Instant,
}

/// Owns the long-lived `tokens.db` connection plus a short-lived cache of
/// resolved tokens so authenticating a request normally never touches SQLite.
pub struct TokenStore {
    conn: Mutex<Connection>,
    cache: Mutex<HashMap<String, CacheEntry>>,
}

impl TokenStore {
    pub fn open(data_dir: &Path) -> Result<Self> {
        Ok(Self {
            conn: Mutex::new(open_tokens_db(data_dir)?),
            cache: Mutex::new(HashMap::new()),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, CacheEntry>> {
        self.cache.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Resolve a presented token value to a principal, or `None` if it is
    /// unknown, expired or revoked.
    pub fn resolve(&self, token: &str) -> Option<Principal> {
        let hash = hash_secret(token);
        let now = unix_now();
        if let Some(entry) = self.cache().get(&hash) {
            let fresh = entry.cached_at.elapsed() < CACHE_TTL;
            let unexpired = entry.expires_at.is_none_or(|e| e > now);
            if fresh && unexpired {
                return Some(entry.principal.clone());
            }
        }
        let row = match lookup_token(&self.conn(), &hash, now) {
            Ok(Some(row)) => row,
            Ok(None) => {
                self.cache().remove(&hash);
                return None;
            }
            Err(e) => {
                tracing::error!("token lookup failed: {e:#}");
                return None;
            }
        };
        let principal = Principal { name: row.name, scope: row.scope };
        self.cache().insert(
            hash,
            CacheEntry { principal: principal.clone(), expires_at: row.expires_at, cached_at: Instant::now() },
        );
        Some(principal)
    }

    pub fn create_invite(
        &self,
        name: &str,
        scope: Scope,
        ttl_secs: Option<u64>,
        token_ttl_secs: Option<u64>,
    ) -> Result<std::result::Result<(String, i64), CreateInviteError>> {
        create_invite(
            &self.conn(),
            name,
            scope,
            ttl_secs.unwrap_or(DEFAULT_INVITE_TTL_SECS),
            token_ttl_secs,
            unix_now(),
        )
    }

    pub fn list_invites(&self) -> Result<Vec<InviteInfo>> {
        list_invites(&self.conn(), unix_now())
    }

    pub fn revoke_invite(&self, id: i64) -> Result<bool> {
        revoke_invite(&self.conn(), id)
    }

    pub fn redeem(&self, code: &str) -> Result<RedeemOutcome> {
        redeem_invite(&mut self.conn(), code, unix_now())
    }

    pub fn list_tokens(&self) -> Result<Vec<TokenInfo>> {
        list_tokens(&self.conn())
    }

    /// Delete a token and drop every cached resolution so revocation is
    /// immediate.
    pub fn revoke_token(&self, name: &str) -> Result<bool> {
        let removed = revoke_token(&self.conn(), name)?;
        if removed {
            self.cache().retain(|_, e| e.principal.name != name);
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn
    }

    fn invite(conn: &Connection, name: &str, scope: Scope, now: i64) -> String {
        create_invite(conn, name, scope, 900, None, now).unwrap().unwrap().0
    }

    #[test]
    fn code_normalization_and_format() {
        assert_eq!(normalize_code("abcd-efgh"), "ABCDEFGH");
        assert_eq!(normalize_code(" 0O1Il "), "00111");
        assert_eq!(format_code("ABCDEFGH"), "ABCD-EFGH");
        let c = generate_code().unwrap();
        assert_eq!(c.len(), CODE_LEN);
        assert!(c.bytes().all(|b| CODE_ALPHABET.contains(&b)));
    }

    #[test]
    fn token_shape() {
        let t = generate_token().unwrap();
        assert!(t.starts_with(TOKEN_PREFIX));
        assert_eq!(t.len(), TOKEN_PREFIX.len() + 64);
        assert_ne!(t, generate_token().unwrap());
    }

    #[test]
    fn redeem_creates_token_with_invite_scope_and_name() {
        let mut conn = mem();
        let code = invite(&conn, "synology1", Scope::UpdateIndex, 1000);
        let RedeemOutcome::Redeemed(r) = redeem_invite(&mut conn, &code, 1100).unwrap() else {
            panic!("expected redeemed")
        };
        assert_eq!(r.name, "synology1");
        assert_eq!(r.scope, Scope::UpdateIndex);
        assert_eq!(r.expires_at, None);
        // Only the hash is stored.
        let stored: String = conn
            .query_row("SELECT token_hash FROM tokens", [], |r| r.get(0))
            .unwrap();
        assert_ne!(stored, r.token);
        assert_eq!(stored, hash_secret(&r.token));
    }

    #[test]
    fn invite_is_single_use() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        assert!(matches!(redeem_invite(&mut conn, &code, 1001).unwrap(), RedeemOutcome::Redeemed(_)));
        assert!(matches!(redeem_invite(&mut conn, &code, 1002).unwrap(), RedeemOutcome::Invalid));
    }

    #[test]
    fn invite_expires() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000); // expires at 1900
        assert!(matches!(redeem_invite(&mut conn, &code, 1900).unwrap(), RedeemOutcome::Invalid));
    }

    #[test]
    fn redeem_accepts_sloppy_input() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        let sloppy = code.to_lowercase().replace('-', " ");
        assert!(matches!(redeem_invite(&mut conn, &sloppy, 1001).unwrap(), RedeemOutcome::Redeemed(_)));
    }

    #[test]
    fn wrong_code_is_invalid_and_does_not_consume() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        assert!(matches!(redeem_invite(&mut conn, "ZZZZ-ZZZZ", 1001).unwrap(), RedeemOutcome::Invalid));
        assert!(matches!(redeem_invite(&mut conn, &code, 1002).unwrap(), RedeemOutcome::Redeemed(_)));
    }

    #[test]
    fn token_ttl_propagates() {
        let mut conn = mem();
        let code = create_invite(&conn, "a", Scope::Read, 900, Some(3600), 1000).unwrap().unwrap().0;
        let RedeemOutcome::Redeemed(r) = redeem_invite(&mut conn, &code, 1100).unwrap() else { panic!() };
        assert_eq!(r.expires_at, Some(4700));
    }

    #[test]
    fn name_uniqueness_enforced_at_creation() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        // Pending invite already holds the name.
        assert_eq!(
            create_invite(&conn, "a", Scope::Read, 900, None, 1000).unwrap(),
            Err(CreateInviteError::NameTaken)
        );
        redeem_invite(&mut conn, &code, 1001).unwrap();
        // Existing token holds it now.
        assert_eq!(
            create_invite(&conn, "a", Scope::Admin, 900, None, 1002).unwrap(),
            Err(CreateInviteError::NameTaken)
        );
        assert_eq!(
            create_invite(&conn, "  ", Scope::Read, 900, None, 1002).unwrap(),
            Err(CreateInviteError::InvalidName)
        );
    }

    #[test]
    fn name_collision_at_redeem_keeps_invite() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        // Simulate a token created out-of-band after the invite was made.
        conn.execute(
            "INSERT INTO tokens (name, token_hash, scope, created_at) VALUES ('a', 'h', 'read', 1)",
            [],
        )
        .unwrap();
        assert!(matches!(redeem_invite(&mut conn, &code, 1001).unwrap(), RedeemOutcome::NameTaken));
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM invites", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "invite must survive a name collision");
    }

    #[test]
    fn list_and_revoke_invites() {
        let conn = mem();
        invite(&conn, "a", Scope::Read, 1000);
        let list = list_invites(&conn, 1001).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "a");
        assert!(revoke_invite(&conn, list[0].id).unwrap());
        assert!(!revoke_invite(&conn, list[0].id).unwrap());
        assert!(list_invites(&conn, 1001).unwrap().is_empty());
        // Expired invites are swept from listings.
        invite(&conn, "b", Scope::Read, 1000);
        assert!(list_invites(&conn, 5000).unwrap().is_empty());
    }

    #[test]
    fn lookup_rejects_expired_and_unknown() {
        let mut conn = mem();
        let code = create_invite(&conn, "a", Scope::Admin, 900, Some(100), 1000).unwrap().unwrap().0;
        let RedeemOutcome::Redeemed(r) = redeem_invite(&mut conn, &code, 1000).unwrap() else { panic!() };
        let h = hash_secret(&r.token);
        let row = lookup_token(&conn, &h, 1050).unwrap().unwrap();
        assert_eq!((row.name.as_str(), row.scope), ("a", Scope::Admin));
        assert!(lookup_token(&conn, &h, 1100).unwrap().is_none(), "expired");
        assert!(lookup_token(&conn, &hash_secret("nope"), 1050).unwrap().is_none());
    }

    #[test]
    fn lookup_throttles_last_used_writes() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        let RedeemOutcome::Redeemed(r) = redeem_invite(&mut conn, &code, 1000).unwrap() else { panic!() };
        let h = hash_secret(&r.token);
        let last = |c: &Connection| -> Option<i64> {
            c.query_row("SELECT last_used_at FROM tokens", [], |r| r.get(0)).unwrap()
        };
        lookup_token(&conn, &h, 1000).unwrap();
        assert_eq!(last(&conn), Some(1000));
        lookup_token(&conn, &h, 1030).unwrap();
        assert_eq!(last(&conn), Some(1000), "within granularity: not rewritten");
        lookup_token(&conn, &h, 1060).unwrap();
        assert_eq!(last(&conn), Some(1060));
    }

    #[test]
    fn revoke_token_removes_it() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        let RedeemOutcome::Redeemed(r) = redeem_invite(&mut conn, &code, 1000).unwrap() else { panic!() };
        assert!(revoke_token(&conn, "a").unwrap());
        assert!(!revoke_token(&conn, "a").unwrap());
        assert!(lookup_token(&conn, &hash_secret(&r.token), 1001).unwrap().is_none());
    }

    #[test]
    fn store_revocation_is_immediate_despite_cache() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = TokenStore::open(dir.path()).unwrap();
        let (code, _) = store.create_invite("a", Scope::UpdateIndex, None, None).unwrap().unwrap();
        let RedeemOutcome::Redeemed(r) = store.redeem(&code).unwrap() else { panic!() };
        let p = store.resolve(&r.token).expect("resolves");
        assert_eq!(p, Principal { name: "a".into(), scope: Scope::UpdateIndex });
        assert!(store.resolve(&r.token).is_some(), "served from cache");
        assert!(store.revoke_token("a").unwrap());
        assert!(store.resolve(&r.token).is_none(), "cache invalidated on revoke");
    }

    #[test]
    fn list_tokens_never_exposes_secrets() {
        let mut conn = mem();
        let code = invite(&conn, "a", Scope::Read, 1000);
        let RedeemOutcome::Redeemed(r) = redeem_invite(&mut conn, &code, 1000).unwrap() else { panic!() };
        let json = serde_json::to_string(&list_tokens(&conn).unwrap()).unwrap();
        assert!(!json.contains(&r.token));
        assert!(!json.contains(&hash_secret(&r.token)));
    }
}
