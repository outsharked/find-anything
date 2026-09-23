use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::Deserialize;

use crate::AppState;

use super::{bearer_token, resolve_token};

#[derive(Deserialize)]
pub struct SessionRequest {
    token: Option<String>,
}

/// POST /api/v1/auth/session
///
/// Validates the provided token and sets an HttpOnly session cookie so that
/// browser-native requests (e.g. `<img src>`) can be authenticated without
/// custom headers. The cookie holds the token that was *presented*, so the
/// session inherits that token's scope.
pub async fn create_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<SessionRequest>,
) -> impl IntoResponse {
    // Accept the token from the JSON body, or fall back to the Authorization header.
    let presented = body.token.as_deref().or_else(|| bearer_token(&headers));

    if resolve_token(&state, presented).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Only reachable with an empty presented value when the server is open
    // (empty root token), in which case the cookie value is never checked.
    let token = presented.unwrap_or("");
    let cookie = format!(
        "find_session={token}; HttpOnly; SameSite=Strict; Path=/"
    );

    (
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie)],
    )
        .into_response()
}

/// DELETE /api/v1/auth/session
///
/// Clears the session cookie.
pub async fn delete_session() -> impl IntoResponse {
    let cookie = "find_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0";
    (
        StatusCode::OK,
        [(axum::http::header::SET_COOKIE, cookie)],
    )
        .into_response()
}
