//! Authentication and authorisation (plan 094).
//!
//! Credentials, in resolution order:
//! 1. the root admin token from `server.toml` (`admin` scope, never revocable
//!    through the API; an empty value disables authentication entirely),
//! 2. the per-process internal token used by the server's own `find-scan`
//!    subprocess (`update-index` scope, in-memory only),
//! 3. named tokens in `tokens.db` (scope and expiry per token).
//!
//! A credential is presented as `Authorization: Bearer <token>` or as the
//! `find_session` cookie. Handlers call [`check_scope`] with the minimum scope
//! the route needs: missing/invalid → 401, valid but insufficient → 403.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::{ConnectInfo, Path, State},
    http::{header::SET_COOKIE, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use find_common::api::{
    CreateInviteRequest, CreateInviteResponse, InvitesResponse, RedeemRequest, Scope,
    TokensResponse,
};

use crate::db::tokens::{CreateInviteError, Principal, RedeemOutcome};
use crate::AppState;

use super::run_blocking;

// ── Credential resolution ──────────────────────────────────────────────────────

/// Constant-time equality for secrets (length may leak; contents do not).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn cookie_token(headers: &HeaderMap) -> Option<&str> {
    let cookies = headers.get("cookie")?.to_str().ok()?;
    cookies
        .split(';')
        .find_map(|part| part.trim().strip_prefix("find_session="))
}

/// Resolve a presented credential to a principal.
///
/// With an empty root token the server is open (e.g. public demo instances):
/// every request, credentialed or not, is an anonymous admin.
pub(crate) fn resolve_token(state: &AppState, presented: Option<&str>) -> Option<Principal> {
    let root = state.config.server.token.as_str();
    if root.is_empty() {
        return Some(Principal { name: "anonymous".into(), scope: Scope::Admin });
    }
    let token = presented?;
    if token.is_empty() {
        return None;
    }
    if ct_eq(token, root) {
        return Some(Principal { name: "root".into(), scope: Scope::Admin });
    }
    if ct_eq(token, &state.internal_token) {
        return Some(Principal { name: "internal".into(), scope: Scope::UpdateIndex });
    }
    state.tokens.resolve(token)
}

/// Authenticate a request and require at least `required` scope.
///
/// Tries the bearer header, then the session cookie; the first credential that
/// resolves wins. Returns 401 if none does, 403 if it does but lacks scope.
pub(crate) fn check_scope(
    state: &AppState,
    headers: &HeaderMap,
    required: Scope,
) -> Result<Principal, StatusCode> {
    let principal = resolve_token(state, bearer_token(headers))
        .or_else(|| resolve_token(state, cookie_token(headers)))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if !principal.scope.allows(required) {
        tracing::warn!(
            token = %principal.name,
            scope = %principal.scope,
            required = %required,
            "request denied: insufficient token scope"
        );
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(principal)
}

// ── Redeem rate limiting ───────────────────────────────────────────────────────

const REDEEM_WINDOW: Duration = Duration::from_secs(60);
/// Failed redeems tolerated per IP per window.
const REDEEM_PER_IP_MAX: u32 = 10;
/// Failed redeems tolerated server-wide per window. Bounds a distributed
/// guess (or a guess through a reverse proxy, where every client shares one
/// peer IP) well below what could enumerate a 40-bit code in its lifetime.
const REDEEM_GLOBAL_MAX: u32 = 30;
/// Above this many tracked IPs, expired entries are pruned.
const REDEEM_PRUNE_AT: usize = 4096;

/// Counts *failed* redeem attempts only, so legitimate redemptions are never
/// throttled by other people's successes.
#[derive(Default)]
pub struct RedeemLimiter {
    per_ip: HashMap<IpAddr, (u32, Instant)>,
    global: Option<(u32, Instant)>,
}

fn live(entry: &(u32, Instant), now: Instant) -> Option<u32> {
    (now.duration_since(entry.1) < REDEEM_WINDOW).then_some(entry.0)
}

impl RedeemLimiter {
    /// True if this IP may attempt a redeem right now.
    pub fn allow(&self, ip: IpAddr, now: Instant) -> bool {
        let ip_fails = self.per_ip.get(&ip).and_then(|e| live(e, now)).unwrap_or(0);
        let global_fails = self.global.as_ref().and_then(|e| live(e, now)).unwrap_or(0);
        ip_fails < REDEEM_PER_IP_MAX && global_fails < REDEEM_GLOBAL_MAX
    }

    pub fn record_failure(&mut self, ip: IpAddr, now: Instant) {
        if self.per_ip.len() >= REDEEM_PRUNE_AT {
            self.per_ip.retain(|_, e| live(e, now).is_some());
        }
        let bump = |slot: &mut (u32, Instant)| {
            if live(slot, now).is_some() {
                slot.0 += 1;
            } else {
                *slot = (1, now);
            }
        };
        bump(self.per_ip.entry(ip).or_insert((0, now)));
        bump(self.global.get_or_insert((0, now)));
    }
}

// ── POST /api/v1/auth/redeem ───────────────────────────────────────────────────

/// Exchange a one-time invite code for a named, scoped token. No
/// authentication required; failed attempts are rate-limited.
pub async fn redeem_invite(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(req): Json<RedeemRequest>,
) -> Response {
    let ip = addr.ip();
    {
        let limiter = state.redeem_limiter.lock().unwrap_or_else(|p| p.into_inner());
        if !limiter.allow(ip, Instant::now()) {
            tracing::warn!(%ip, "invite redemption rate-limited");
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        }
    }

    run_blocking("redeem_invite", move || {
        Ok(match state.tokens.redeem(&req.code)? {
            RedeemOutcome::Redeemed(resp) => {
                tracing::info!(token = %resp.name, scope = %resp.scope, %ip, "invite redeemed");
                let cookie = format!(
                    "find_session={}; HttpOnly; SameSite=Strict; Path=/",
                    resp.token
                );
                ([(SET_COOKIE, cookie)], Json(resp)).into_response()
            }
            RedeemOutcome::Invalid => {
                state
                    .redeem_limiter
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .record_failure(ip, Instant::now());
                tracing::warn!(%ip, "invite redemption failed: invalid or expired code");
                StatusCode::UNAUTHORIZED.into_response()
            }
            RedeemOutcome::NameTaken => StatusCode::CONFLICT.into_response(),
        })
    })
    .await
}

// ── Admin: invites and tokens ──────────────────────────────────────────────────

/// POST /api/v1/admin/invites
pub async fn create_invite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CreateInviteRequest>,
) -> Response {
    let principal = match check_scope(&state, &headers, Scope::Admin) {
        Ok(p) => p,
        Err(s) => return s.into_response(),
    };
    run_blocking("create_invite", move || {
        Ok(match state.tokens.create_invite(&req.name, req.scope, req.ttl_secs, req.token_ttl_secs)? {
            Ok((code, expires_at)) => {
                tracing::info!(
                    by = %principal.name, name = %req.name, scope = %req.scope,
                    "invite created"
                );
                Json(CreateInviteResponse { code, expires_at }).into_response()
            }
            Err(CreateInviteError::NameTaken) => StatusCode::CONFLICT.into_response(),
            Err(CreateInviteError::InvalidName) => StatusCode::BAD_REQUEST.into_response(),
        })
    })
    .await
}

/// GET /api/v1/admin/invites
pub async fn list_invites(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(s) = check_scope(&state, &headers, Scope::Admin) {
        return s.into_response();
    }
    run_blocking("list_invites", move || {
        Ok(Json(InvitesResponse { invites: state.tokens.list_invites()? }))
    })
    .await
}

/// DELETE /api/v1/admin/invites/{id}
pub async fn revoke_invite(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> Response {
    let principal = match check_scope(&state, &headers, Scope::Admin) {
        Ok(p) => p,
        Err(s) => return s.into_response(),
    };
    run_blocking("revoke_invite", move || {
        Ok(if state.tokens.revoke_invite(id)? {
            tracing::info!(by = %principal.name, id, "invite revoked");
            StatusCode::NO_CONTENT
        } else {
            StatusCode::NOT_FOUND
        })
    })
    .await
}

/// GET /api/v1/admin/tokens
pub async fn list_tokens(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Err(s) = check_scope(&state, &headers, Scope::Admin) {
        return s.into_response();
    }
    run_blocking("list_tokens", move || {
        Ok(Json(TokensResponse { tokens: state.tokens.list_tokens()? }))
    })
    .await
}

/// DELETE /api/v1/admin/tokens/{name}
pub async fn revoke_token(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    let principal = match check_scope(&state, &headers, Scope::Admin) {
        Ok(p) => p,
        Err(s) => return s.into_response(),
    };
    run_blocking("revoke_token", move || {
        Ok(if state.tokens.revoke_token(&name)? {
            tracing::info!(by = %principal.name, token = %name, "token revoked");
            StatusCode::NO_CONTENT
        } else {
            StatusCode::NOT_FOUND
        })
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_basics() {
        assert!(ct_eq("abc", "abc"));
        assert!(!ct_eq("abc", "abd"));
        assert!(!ct_eq("abc", "abcd"));
        assert!(ct_eq("", ""));
    }

    #[test]
    fn cookie_and_bearer_extraction() {
        let mut h = HeaderMap::new();
        h.insert("Authorization", "Bearer tok".parse().unwrap());
        h.insert("cookie", "a=b; find_session=sess; c=d".parse().unwrap());
        assert_eq!(bearer_token(&h), Some("tok"));
        assert_eq!(cookie_token(&h), Some("sess"));
        assert_eq!(bearer_token(&HeaderMap::new()), None);
    }

    #[test]
    fn redeem_limiter_counts_failures_per_ip_and_globally() {
        let now = Instant::now();
        let mut l = RedeemLimiter::default();
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        for _ in 0..REDEEM_PER_IP_MAX {
            assert!(l.allow(a, now));
            l.record_failure(a, now);
        }
        assert!(!l.allow(a, now), "ip blocked after too many failures");
        assert!(l.allow(b, now), "other ips unaffected");
        // Window expiry unblocks.
        assert!(l.allow(a, now + REDEEM_WINDOW));
    }

    #[test]
    fn redeem_limiter_global_cap_blocks_everyone() {
        let now = Instant::now();
        let mut l = RedeemLimiter::default();
        for i in 0..REDEEM_GLOBAL_MAX {
            let ip: IpAddr = format!("10.1.{}.{}", i / 250, i % 250).parse().unwrap();
            l.record_failure(ip, now);
        }
        assert!(!l.allow("192.168.0.1".parse().unwrap(), now));
    }
}
