//! Dashboard static assets embedded in the binary, plus the ticket login.

use crate::auth;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Redirect, Response};
use serde::Deserialize;
use std::sync::Arc;

#[derive(rust_embed::RustEmbed)]
#[folder = "../../apps/dashboard/dist"]
#[allow_missing = true]
struct Dist;

fn serve_file(path: &str) -> Response {
    match Dist::get(path) {
        Some(f) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            let mut r = ([(header::CONTENT_TYPE, mime.as_ref().to_string())], f.data.into_owned()).into_response();
            r.headers_mut().insert("content-security-policy", "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
            r
        }
        None => match Dist::get("index.html") {
            Some(f) => {
                let mut r = Html(f.data.into_owned()).into_response();
                r.headers_mut().insert("content-security-policy", "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'".parse().unwrap());
                r
            }
            None => (StatusCode::NOT_FOUND, Html("<h1>Prompt VPN</h1><p>Dashboard assets were not built into this binary. Run <code>npm run build</code> in apps/dashboard and rebuild.</p>")).into_response(),
        },
    }
}

pub async fn index() -> Response {
    serve_file("index.html")
}

pub async fn file(Path(path): Path<String>) -> Response {
    serve_file(&path)
}

#[derive(Deserialize)]
pub struct Ticket {
    ticket: String,
}

/// One-time ticket → HttpOnly SameSite=Strict session cookie.
pub async fn login(State(st): State<Arc<AppState>>, Query(t): Query<Ticket>) -> Response {
    match st.sessions.redeem(&t.ticket) {
        Some((id, _)) => {
            let mut r = Redirect::to("/dashboard/").into_response();
            r.headers_mut().insert(header::SET_COOKIE, auth::session_cookie(&id).parse().unwrap());
            r
        }
        None => (StatusCode::UNAUTHORIZED, Html("<h1>Login link expired</h1><p>Run <code>promptvpn dashboard</code> again.</p>")).into_response(),
    }
}
