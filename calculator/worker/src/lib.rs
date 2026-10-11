//! The calculator's shell on Cloudflare Workers: moves data between the
//! kernel, PostgreSQL (through Hyperdrive) and JSON. It decides nothing.
//!
//! `POST /rpc` takes a JSON command with a bearer token, as the h5i
//! calculator server does; `GET /healthz` answers `ok`.

pub mod auth;
pub mod calc;
pub mod engine;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use calc::Calc;
use engine::{Api, DbError, Engine};
use h5i_app_json::Value;
use std::sync::Arc;
use tower_service::Service;
use worker::{Context, Env, HttpRequest, event};

/// PostgreSQL schema of the app's tables, as the h5i calculator server uses.
pub const SCHEMA: &str = "calculator";

#[derive(Clone)]
struct AppState {
    env: Arc<Env>,
}

fn router(env: Env) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/rpc", post(rpc))
        .with_state(AppState { env: Arc::new(env) })
}

#[event(fetch)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    Ok(router(env).call(req).await?)
}

#[worker::send]
async fn rpc(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let secret = match state.env.secret("H5I_APP_SECRET") {
        Ok(s) => s.to_string(),
        Err(e) => return unavailable(&e),
    };
    let auth = auth::HmacAuth::new(secret);
    let (org, user) = match auth.authenticate(&headers, engine::now().secs()) {
        Ok(p) => p,
        Err(auth::AuthError(m)) => {
            if m == "bad signature" {
                worker::console_log!("bad signature; H5I_APP_SECRET is {}", auth.fingerprint());
            }
            return reply(StatusCode::UNAUTHORIZED, error_body(&m));
        }
    };
    let actor = calc::principal(org, user);
    let cmd = match Calc::decode_command(&body) {
        Ok(c) => c,
        Err(m) => return reply(StatusCode::BAD_REQUEST, error_body(&m)),
    };
    let hyperdrive = match state.env.hyperdrive("HYPERDRIVE") {
        Ok(h) => h,
        Err(e) => return unavailable(&e),
    };
    let mut engine = match Engine::<Calc, Calc>::connect(hyperdrive, SCHEMA).await {
        Ok(e) => e,
        Err(e) => return unavailable(&e),
    };
    let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    let result = match key {
        Some(k) => engine.execute_idempotent(&actor, k, &cmd).await,
        None => engine.execute(&actor, &cmd).await,
    };
    match result {
        Ok(Ok(r)) => reply(StatusCode::OK, Calc::encode_reply(&r)),
        Ok(Err(refusal)) => {
            let (status, body) = Calc::encode_error(&refusal);
            reply(status, body)
        }
        Err(DbError::IdempotencyConflict) => reply(StatusCode::UNPROCESSABLE_ENTITY, error_body("idempotency key reused")),
        Err(e) => unavailable(&e),
    }
}

fn unavailable(e: &dyn std::fmt::Display) -> Response {
    worker::console_error!("request failed: {e}");
    reply(StatusCode::SERVICE_UNAVAILABLE, error_body("unavailable"))
}

/// Every response body goes through the verified writer (`h5i-app-json`).
pub fn reply(status: StatusCode, body: Value) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body.to_bytes()).into_response()
}

/// `{"error": <message>}`.
pub fn error_body(msg: &str) -> Value {
    Value::obj([("error", Value::str(msg))])
}
