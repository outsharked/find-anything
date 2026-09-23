//! Scoped access tokens and invite enrollment (plan 094).

mod helpers;
use helpers::{make_text_bulk, TestServer, TEST_TOKEN};

use find_common::api::{
    CreateInviteRequest, CreateInviteResponse, InvitesResponse, RedeemResponse, Scope,
    TokensResponse,
};
use reqwest::StatusCode;

// ── helpers ───────────────────────────────────────────────────────────────────

async fn create_invite_with(
    srv: &TestServer,
    name: &str,
    scope: Scope,
    ttl_secs: Option<u64>,
    token_ttl_secs: Option<u64>,
) -> reqwest::Response {
    srv.client
        .post(srv.url("/api/v1/admin/invites"))
        .json(&CreateInviteRequest { name: name.into(), scope, ttl_secs, token_ttl_secs })
        .send()
        .await
        .unwrap()
}

async fn invite(srv: &TestServer, name: &str, scope: Scope) -> String {
    let resp = create_invite_with(srv, name, scope, None, None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json::<CreateInviteResponse>().await.unwrap().code
}

async fn redeem_raw(srv: &TestServer, code: &str) -> reqwest::Response {
    srv.anonymous_client()
        .post(srv.url("/api/v1/auth/redeem"))
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .unwrap()
}

async fn redeem(srv: &TestServer, code: &str) -> RedeemResponse {
    let resp = redeem_raw(srv, code).await;
    assert_eq!(resp.status(), StatusCode::OK);
    resp.json().await.unwrap()
}

/// Mint a token of the given scope and return a client presenting it.
async fn token_client(srv: &TestServer, name: &str, scope: Scope) -> (reqwest::Client, String) {
    let code = invite(srv, name, scope).await;
    let r = redeem(srv, &code).await;
    (srv.client_with_token(&r.token), r.token)
}

async fn status(c: &reqwest::Client, srv: &TestServer, method: reqwest::Method, path: &str) -> StatusCode {
    c.request(method, srv.url(path)).send().await.unwrap().status()
}

// ── scope enforcement ─────────────────────────────────────────────────────────

#[tokio::test]
async fn no_or_bad_credentials_are_401() {
    let srv = TestServer::spawn().await;
    let anon = srv.anonymous_client();
    assert_eq!(status(&anon, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::UNAUTHORIZED);
    let bad = srv.client_with_token("fa_not_a_real_token");
    assert_eq!(status(&bad, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::UNAUTHORIZED);
    assert_eq!(status(&bad, &srv, reqwest::Method::GET, "/api/v1/admin/inbox").await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn read_token_can_read_but_not_write_or_admin() {
    let srv = TestServer::spawn().await;
    let (c, _) = token_client(&srv, "reader", Scope::Read).await;

    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::OK);
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/settings").await, StatusCode::OK);
    // update-index route → 403 (not 401): valid token, insufficient scope.
    assert_eq!(status(&c, &srv, reqwest::Method::POST, "/api/v1/bulk").await, StatusCode::FORBIDDEN);
    assert_eq!(status(&c, &srv, reqwest::Method::HEAD, "/api/v1/upload/some-id").await, StatusCode::FORBIDDEN);
    // admin routes → 403.
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/admin/inbox").await, StatusCode::FORBIDDEN);
    assert_eq!(status(&c, &srv, reqwest::Method::DELETE, "/api/v1/admin/source?source=x").await, StatusCode::FORBIDDEN);
    assert_eq!(status(&c, &srv, reqwest::Method::POST, "/api/v1/admin/update/apply").await, StatusCode::FORBIDDEN);
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/admin/tokens").await, StatusCode::FORBIDDEN);
    let resp = c
        .post(srv.url("/api/v1/admin/invites"))
        .json(&CreateInviteRequest { name: "x".into(), scope: Scope::Admin, ttl_secs: None, token_ttl_secs: None })
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN, "a read token must not mint an admin invite");
}

#[tokio::test]
async fn update_index_token_can_index_and_read_but_not_admin() {
    let srv = TestServer::spawn().await;
    let (c, _) = token_client(&srv, "scanner", Scope::UpdateIndex).await;

    // Hierarchical: update-index ⊇ read.
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::OK);

    // Can submit a bulk request.
    let req = make_text_bulk("docs", "a.txt", "hello");
    let json = serde_json::to_vec(&req).unwrap();
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &json).unwrap();
    let gz = enc.finish().unwrap();
    let resp = c
        .post(srv.url("/api/v1/bulk"))
        .header("Content-Encoding", "gzip")
        .body(gz)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::ACCEPTED);
    srv.wait_for_idle().await;

    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/admin/inbox").await, StatusCode::FORBIDDEN);
    assert_eq!(status(&c, &srv, reqwest::Method::POST, "/api/v1/admin/compact").await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admin_token_can_do_everything() {
    let srv = TestServer::spawn().await;
    let (c, _) = token_client(&srv, "ops", Scope::Admin).await;
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/admin/inbox").await, StatusCode::OK);
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/admin/tokens").await, StatusCode::OK);
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::OK);
}

#[tokio::test]
async fn root_token_still_works_as_admin() {
    let srv = TestServer::spawn().await;
    assert_eq!(
        status(&srv.client_with_token(TEST_TOKEN), &srv, reqwest::Method::GET, "/api/v1/admin/inbox").await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn empty_root_token_means_open_access() {
    let srv = TestServer::spawn_open().await;
    let anon = srv.anonymous_client();
    assert_eq!(status(&anon, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::OK);
    assert_eq!(status(&anon, &srv, reqwest::Method::GET, "/api/v1/admin/inbox").await, StatusCode::OK);
}

// ── revocation and expiry ─────────────────────────────────────────────────────

#[tokio::test]
async fn revoked_token_is_rejected_immediately() {
    let srv = TestServer::spawn().await;
    let (c, _) = token_client(&srv, "doomed", Scope::Read).await;
    // Prime the server-side resolve cache.
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::OK);

    let resp = srv.client.delete(srv.url("/api/v1/admin/tokens/doomed")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::UNAUTHORIZED);

    // Revoking again → 404.
    let resp = srv.client.delete(srv.url("/api/v1/admin/tokens/doomed")).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn expired_token_is_rejected() {
    let srv = TestServer::spawn().await;
    // Timestamps have whole-second granularity, so a 1 s lifetime can already be
    // over by the first request; 2 s guarantees at least one valid second.
    let resp = create_invite_with(&srv, "shortlived", Scope::Read, None, Some(2)).await;
    let code = resp.json::<CreateInviteResponse>().await.unwrap().code;
    let r = redeem(&srv, &code).await;
    assert!(r.expires_at.is_some());
    let c = srv.client_with_token(&r.token);

    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::OK);
    tokio::time::sleep(std::time::Duration::from_millis(3100)).await;
    // Rejected even though the resolution above is cached.
    assert_eq!(status(&c, &srv, reqwest::Method::GET, "/api/v1/sources").await, StatusCode::UNAUTHORIZED);
}

// ── invites ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn invite_is_single_use_and_carries_scope_and_name() {
    let srv = TestServer::spawn().await;
    let code = invite(&srv, "synology1", Scope::UpdateIndex).await;
    let r = redeem(&srv, &code).await;
    assert_eq!(r.name, "synology1");
    assert_eq!(r.scope, Scope::UpdateIndex);
    assert!(r.token.starts_with("fa_"));

    assert_eq!(redeem_raw(&srv, &code).await.status(), StatusCode::UNAUTHORIZED, "second redeem");
}

#[tokio::test]
async fn expired_invite_cannot_be_redeemed() {
    let srv = TestServer::spawn().await;
    let resp = create_invite_with(&srv, "late", Scope::Read, Some(1), None).await;
    let code = resp.json::<CreateInviteResponse>().await.unwrap().code;
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    assert_eq!(redeem_raw(&srv, &code).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_code_is_401() {
    let srv = TestServer::spawn().await;
    assert_eq!(redeem_raw(&srv, "ZZZZ-ZZZZ").await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn redeem_accepts_lowercase_and_no_dash() {
    let srv = TestServer::spawn().await;
    let code = invite(&srv, "sloppy", Scope::Read).await;
    let sloppy = code.replace('-', "").to_lowercase();
    assert_eq!(redeem_raw(&srv, &sloppy).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn duplicate_name_is_409_and_bad_name_is_400() {
    let srv = TestServer::spawn().await;
    invite(&srv, "dup", Scope::Read).await; // pending invite holds the name
    assert_eq!(
        create_invite_with(&srv, "dup", Scope::Read, None, None).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        create_invite_with(&srv, "  ", Scope::Read, None, None).await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn concurrent_redeem_yields_exactly_one_token() {
    let srv = TestServer::spawn().await;
    let code = invite(&srv, "race", Scope::Read).await;
    let (a, b, c, d, e, f) = tokio::join!(
        redeem_raw(&srv, &code),
        redeem_raw(&srv, &code),
        redeem_raw(&srv, &code),
        redeem_raw(&srv, &code),
        redeem_raw(&srv, &code),
        redeem_raw(&srv, &code),
    );
    let statuses: Vec<StatusCode> =
        [a, b, c, d, e, f].iter().map(|r| r.status()).collect();
    let ok = statuses.iter().filter(|s| **s == StatusCode::OK).count();
    assert_eq!(ok, 1, "exactly one redemption may succeed: {statuses:?}");
    let tokens: TokensResponse = srv
        .client
        .get(srv.url("/api/v1/admin/tokens"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tokens.tokens.len(), 1);
}

#[tokio::test]
async fn failed_redeems_are_rate_limited() {
    let srv = TestServer::spawn().await;
    let good = invite(&srv, "victim", Scope::Read).await;
    let mut saw_429 = false;
    for _ in 0..15 {
        if redeem_raw(&srv, "ZZZZ-ZZZZ").await.status() == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "repeated failures must eventually be throttled");
    // Once throttled, even the correct code is refused until the window passes.
    assert_eq!(redeem_raw(&srv, &good).await.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn successful_redeems_are_not_throttled() {
    let srv = TestServer::spawn().await;
    for i in 0..12 {
        let code = invite(&srv, &format!("c{i}"), Scope::Read).await;
        assert_eq!(redeem_raw(&srv, &code).await.status(), StatusCode::OK);
    }
}

// ── listing / management ──────────────────────────────────────────────────────

#[tokio::test]
async fn list_and_revoke_invites() {
    let srv = TestServer::spawn().await;
    let code = invite(&srv, "pending", Scope::Read).await;
    let list: InvitesResponse = srv
        .client
        .get(srv.url("/api/v1/admin/invites"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.invites.len(), 1);
    assert_eq!(list.invites[0].name, "pending");
    // The code itself is never listed.
    assert!(!serde_json::to_string(&list).unwrap().contains(&code.replace('-', "")));

    let id = list.invites[0].id;
    let resp = srv.client.delete(srv.url(&format!("/api/v1/admin/invites/{id}"))).send().await.unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert_eq!(redeem_raw(&srv, &code).await.status(), StatusCode::UNAUTHORIZED, "revoked invite");
}

#[tokio::test]
async fn token_list_never_exposes_token_values() {
    let srv = TestServer::spawn().await;
    let (_, token) = token_client(&srv, "secretive", Scope::Read).await;
    let body = srv
        .client
        .get(srv.url("/api/v1/admin/tokens"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("secretive"));
    assert!(!body.contains(&token));
}

// ── sessions / cookies ────────────────────────────────────────────────────────

fn set_cookie_value(resp: &reqwest::Response) -> String {
    let raw = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .expect("Set-Cookie header")
        .to_str()
        .unwrap();
    raw.split(';').next().unwrap().to_string() // "find_session=<token>"
}

#[tokio::test]
async fn redeem_sets_session_cookie_that_inherits_scope() {
    let srv = TestServer::spawn().await;
    let code = invite(&srv, "browser", Scope::Read).await;
    let resp = redeem_raw(&srv, &code).await;
    let cookie = set_cookie_value(&resp);
    let r: RedeemResponse = resp.json().await.unwrap();
    assert_eq!(cookie, format!("find_session={}", r.token));

    let anon = srv.anonymous_client();
    let get = |path: &'static str| {
        let anon = anon.clone();
        let url = srv.url(path);
        let cookie = cookie.clone();
        async move { anon.get(url).header("Cookie", cookie).send().await.unwrap().status() }
    };
    assert_eq!(get("/api/v1/sources").await, StatusCode::OK);
    assert_eq!(get("/api/v1/admin/inbox").await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn session_created_from_header_stores_presented_token_not_root() {
    let srv = TestServer::spawn().await;
    let (c, token) = token_client(&srv, "reader", Scope::Read).await;
    let resp = c
        .post(srv.url("/api/v1/auth/session"))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let cookie = set_cookie_value(&resp);
    assert_eq!(cookie, format!("find_session={token}"));
    assert!(!cookie.contains(TEST_TOKEN), "root token must never leak into the cookie");

    // Body token variant, and rejection of a bad one.
    let resp = srv
        .anonymous_client()
        .post(srv.url("/api/v1/auth/session"))
        .json(&serde_json::json!({ "token": token }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let resp = srv
        .anonymous_client()
        .post(srv.url("/api/v1/auth/session"))
        .json(&serde_json::json!({ "token": "fa_bogus" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

// ── link routes are unaffected ────────────────────────────────────────────────

#[tokio::test]
async fn read_token_can_mint_share_links_but_public_resolve_needs_no_token() {
    let srv = TestServer::spawn().await;
    srv.post_bulk(&make_text_bulk("docs", "n.txt", "x")).await;
    srv.wait_for_idle().await;
    let (c, _) = token_client(&srv, "reader", Scope::Read).await;

    let resp = c
        .post(srv.url("/api/v1/links"))
        .json(&serde_json::json!({ "source": "docs", "path": "n.txt" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let code = resp.json::<serde_json::Value>().await.unwrap()["code"].as_str().unwrap().to_string();

    let resp = srv
        .anonymous_client()
        .get(srv.url(&format!("/api/v1/links/{code}")))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
