//! The local review page. Serves one plan on 127.0.0.1, waits for exactly one
//! submission, then returns it.

use std::{sync::Arc, time::Duration};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rust_embed::RustEmbed;
use serde::Serialize;
use tokio::sync::{Mutex, Notify, oneshot};

use crate::hook::Submission;

#[derive(RustEmbed)]
#[folder = "ui/"]
struct Ui;

const TOKEN_HEADER: &str = "x-planos-token";
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; \
                   img-src 'self' data: https:; connect-src 'self'; object-src 'none'; \
                   base-uri 'none'; frame-ancestors 'none'";

#[derive(Clone, Serialize)]
pub struct Plan {
    pub title: String,
    pub path: Option<String>,
    pub markdown: String,
    pub html: String,
}

pub struct Review {
    plan: Plan,
    token: String,
    done: Mutex<Option<oneshot::Sender<Submission>>>,
}

impl Review {
    pub fn new(plan: Plan, token: String) -> (Arc<Self>, oneshot::Receiver<Submission>) {
        let (tx, rx) = oneshot::channel();
        let review = Self { plan, token, done: Mutex::new(Some(tx)) };
        (Arc::new(review), rx)
    }
}

pub fn router(review: Arc<Review>) -> Router {
    Router::new()
        .route("/", get(|| asset("index.html".to_string())))
        .route("/assets/{*file}", get(|Path(file): Path<String>| asset(file)))
        .route("/api/plan", get(plan))
        .route("/api/submit", post(submit))
        .with_state(review)
}

/// Random hex token. The page must echo it in a custom header, so another
/// site in the same browser cannot read the plan or submit a decision.
pub fn token() -> Result<String> {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("random token: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Serve the review until it is submitted, then shut down cleanly so the
/// browser gets its response before the process exits.
pub async fn run(review: Arc<Review>, rx: oneshot::Receiver<Submission>, open: bool) -> Result<Submission> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.context("bind 127.0.0.1")?;
    let url = format!("http://{}/?t={}", listener.local_addr()?, review.token);
    eprintln!("PlanOS: review the plan at {url}");
    if let Some(f) = std::env::var_os("PLANOS_URL_FILE") {
        std::fs::write(&f, &url).with_context(|| format!("write {}", f.to_string_lossy()))?;
    }
    if open {
        open_browser(&url);
    }

    let stop = Arc::new(Notify::new());
    let stop_wait = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(review))
            .with_graceful_shutdown(async move { stop_wait.notified().await })
            .await
    });

    let submission = rx.await.context("review closed without a decision")?;
    stop.notify_one();
    let _ = tokio::time::timeout(Duration::from_secs(3), server).await;
    Ok(submission)
}

async fn plan(State(r): State<Arc<Review>>, headers: HeaderMap) -> Response {
    if !authorized(&r, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    Json(r.plan.clone()).into_response()
}

async fn submit(State(r): State<Arc<Review>>, headers: HeaderMap, Json(sub): Json<Submission>) -> Response {
    if !authorized(&r, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match r.done.lock().await.take() {
        Some(tx) => {
            let _ = tx.send(sub);
            StatusCode::NO_CONTENT.into_response()
        }
        None => (StatusCode::CONFLICT, "already submitted").into_response(),
    }
}

fn authorized(r: &Review, headers: &HeaderMap) -> bool {
    headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) == Some(r.token.as_str())
}

async fn asset(path: String) -> Response {
    let Some(file) = Ui::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mut res = file.data.into_owned().into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap());
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    res
}

fn open_browser(url: &str) {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Err(e) = cmd.spawn() {
        eprintln!("PlanOS: could not open a browser ({e}). Open the URL above.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn review() -> (Arc<Review>, oneshot::Receiver<Submission>) {
        let plan = Plan {
            title: "T".into(),
            path: Some("/p.md".into()),
            markdown: "# T".into(),
            html: "<h1>T</h1>".into(),
        };
        Review::new(plan, "tok".into())
    }

    fn req(method: &str, uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
        let mut b = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header(TOKEN_HEADER, t);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn api_rejects_missing_or_wrong_token() {
        let (r, _rx) = review();
        for t in [None, Some("nope")] {
            let res = router(r.clone()).oneshot(req("GET", "/api/plan", t, "")).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
            let res = router(r.clone())
                .oneshot(req("POST", "/api/submit", t, r#"{"decision":"approve"}"#))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn plan_is_served_with_token() {
        let (r, _rx) = review();
        let res = router(r).oneshot(req("GET", "/api/plan", Some("tok"), "")).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["title"], "T");
        assert_eq!(v["html"], "<h1>T</h1>");
    }

    #[tokio::test]
    async fn first_submission_wins_second_conflicts() {
        let (r, rx) = review();
        let body = r#"{"decision":"changes","comments":[],"general":"split it"}"#;
        let res = router(r.clone()).oneshot(req("POST", "/api/submit", Some("tok"), body)).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            rx.await.unwrap(),
            Submission::Changes { comments: vec![], general: "split it".into() }
        );
        let res = router(r)
            .oneshot(req("POST", "/api/submit", Some("tok"), r#"{"decision":"approve"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn index_and_assets_have_csp() {
        let (r, _rx) = review();
        for uri in ["/", "/assets/app.js", "/assets/app.css"] {
            let res = router(r.clone()).oneshot(req("GET", uri, None, "")).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri}");
            assert!(res.headers().contains_key(header::CONTENT_SECURITY_POLICY));
        }
        let res = router(r).oneshot(req("GET", "/assets/nope.js", None, "")).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn tokens_are_32_hex_and_differ() {
        let a = token().unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, token().unwrap());
    }
}
