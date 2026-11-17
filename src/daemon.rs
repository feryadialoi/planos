//! `planos serve`: the local service behind every PlanOS hook and page.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, patch, post},
};
use rust_embed::RustEmbed;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, watch};

use crate::hook::{self, Decision};
use crate::render;
use crate::store::{CommentState, Pending, Session, Status, Store, now_ms};

#[derive(RustEmbed)]
#[folder = "ui/"]
struct Ui;

pub const TOKEN_HEADER: &str = "x-planos-token";
// Mermaid styles its SVG inline, so styles allow 'unsafe-inline'; scripts stay 'self' only.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data: https:; connect-src 'self'; object-src 'none'; \
                   base-uri 'none'; frame-ancestors 'none'";
const POLL: Duration = Duration::from_secs(25);
/// A waiting hook that stops polling this long is gone (answered in the terminal, or killed).
const STALE: Duration = Duration::from_secs(60);
/// A page that polled this recently is still open.
const VIEWER: Duration = Duration::from_secs(10);

pub struct App {
    pub token: String,
    store: Mutex<Inner>,
    version: watch::Sender<u64>,
}

struct Inner {
    store: Store,
    decisions: HashMap<u64, Decision>,
    /// Last poll from each waiting hook, by request id.
    waiting: HashMap<u64, Instant>,
    /// Long-polls in flight, by request id. A request with one is alive.
    polls: HashMap<u64, usize>,
    /// Last poll from a page, by session id ("*" for the dashboard).
    viewers: HashMap<String, Instant>,
    next_request: u64,
}

impl App {
    pub fn new(home: &std::path::Path, token: String) -> Result<Arc<Self>> {
        let store = Store::open(home)?;
        let (version, _) = watch::channel(1);
        Ok(Arc::new(Self {
            token,
            store: Mutex::new(Inner { store, decisions: HashMap::new(), waiting: HashMap::new(), polls: HashMap::new(), viewers: HashMap::new(), next_request: now_ms() }),
            version,
        }))
    }

    fn bump(&self) {
        self.version.send_modify(|v| *v += 1);
    }
}

pub async fn serve(home: PathBuf, port: u16, token: String) -> Result<()> {
    let app = App::new(&home, token)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.with_context(|| format!("bind 127.0.0.1:{port}"))?;
    eprintln!("PlanOS {} serving on http://127.0.0.1:{port}", env!("CARGO_PKG_VERSION"));

    let sweeper = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            sweeper.sweep().await;
        }
    });
    axum::serve(listener, router(app)).await?;
    Ok(())
}

impl App {
    /// Drop reviews and questions whose hook stopped polling.
    async fn sweep(&self) {
        self.drop_idle(None, STALE).await;
    }

    /// Drop waiting requests (all, or just `only`) with no poll for `idle`.
    async fn drop_idle(&self, only: Option<u64>, idle: Duration) {
        let mut g = self.store.lock().await;
        let inner = &mut *g;
        let now = Instant::now();
        let stale: Vec<String> = inner
            .store
            .sessions
            .values()
            .filter_map(|s| {
                let p = s.pending.as_ref()?;
                if only.is_some_and(|rid| rid != p.id()) || inner.polls.get(&p.id()).is_some_and(|n| *n > 0) {
                    return None;
                }
                let dead = match inner.waiting.get(&p.id()) {
                    Some(t) => now.duration_since(*t) >= idle,
                    None => now_ms().saturating_sub(pending_since(p)) >= idle.as_millis() as u64,
                };
                dead.then(|| s.id.clone())
            })
            .collect();
        for id in &stale {
            if let Some(s) = inner.store.sessions.get_mut(id) {
                if let Some(p) = s.pending.take() {
                    inner.waiting.remove(&p.id());
                }
                if s.status == Status::Review {
                    s.status = Status::Drafting;
                }
                s.touch();
            }
            let _ = inner.store.save(id);
        }
        drop(g);
        if !stale.is_empty() {
            self.bump();
        }
    }
}

fn pending_since(p: &Pending) -> u64 {
    match p {
        Pending::Review { since, .. } | Pending::Question { since, .. } => *since,
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(|| asset("index.html".into())))
        .route("/assets/{*file}", get(|Path(f): Path<String>| asset(f)))
        .route("/vendor/{*file}", get(|Path(f): Path<String>| asset(format!("vendor/{f}"))))
        .route("/api/health", get(health))
        .route("/api/shutdown", post(shutdown))
        .route("/api/hook/review", post(hook_review))
        .route("/api/hook/ask", post(hook_ask))
        .route("/api/hook/wait/{rid}", get(hook_wait))
        .route("/api/hook/event", post(hook_event))
        .route("/api/hook/stop", post(hook_stop))
        .route("/api/hook/prompt", post(hook_prompt))
        .route("/api/sessions", get(sessions))
        .route("/api/s/{id}", get(session).delete(delete_session))
        .route("/api/s/{id}/round/{n}", get(round))
        .route("/api/s/{id}/comments", post(add_comment))
        .route("/api/s/{id}/comments/{cid}", patch(edit_comment))
        .route("/api/s/{id}/comments/{cid}", delete(delete_comment))
        .route("/api/s/{id}/decide", post(decide))
        .with_state(app)
}

fn authorized(app: &App, headers: &HeaderMap) -> bool {
    headers.get(TOKEN_HEADER).and_then(|v| v.to_str().ok()) == Some(app.token.as_str())
}

macro_rules! guard {
    ($app:expr, $headers:expr) => {
        if !authorized(&$app, &$headers) {
            return StatusCode::FORBIDDEN.into_response();
        }
    };
}

fn err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

async fn health(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    guard!(app, headers);
    Json(json!({ "version": env!("CARGO_PKG_VERSION") })).into_response()
}

async fn shutdown(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    guard!(app, headers);
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::process::exit(0);
    });
    StatusCode::NO_CONTENT.into_response()
}

// ---------- hooks ----------

#[derive(Deserialize)]
struct HookCall {
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    tool_input: Value,
    /// Plan text the client read (ExitPlanMode or a plan file write).
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    plan_path: Option<String>,
}

async fn hook_review(State(app): State<Arc<App>>, headers: HeaderMap, Json(call): Json<HookCall>) -> Response {
    guard!(app, headers);
    let Some(plan) = call.plan.clone() else { return err(StatusCode::BAD_REQUEST, "no plan") };
    let mut g = app.store.lock().await;
    g.settle_elsewhere(&call.session_id);
    g.next_request += 1;
    let rid = g.next_request;
    let s = g.store.session(&call.session_id, &call.cwd);
    s.present(plan, call.plan_path.clone(), rid, call.tool_input.clone());
    let _ = g.store.save(&call.session_id);
    g.waiting.insert(rid, Instant::now());
    let open = !has_viewer(&g.viewers, &call.session_id);
    drop(g);
    app.bump();
    Json(json!({ "request_id": rid, "open": open, "path": format!("/#/s/{}", call.session_id) })).into_response()
}

async fn hook_ask(State(app): State<Arc<App>>, headers: HeaderMap, Json(call): Json<HookCall>) -> Response {
    guard!(app, headers);
    let mut g = app.store.lock().await;
    g.settle_elsewhere(&call.session_id);
    g.next_request += 1;
    let rid = g.next_request;
    let s = g.store.session(&call.session_id, &call.cwd);
    s.pending = Some(Pending::Question { id: rid, tool_input: call.tool_input.clone(), since: now_ms() });
    s.touch();
    let _ = g.store.save(&call.session_id);
    g.waiting.insert(rid, Instant::now());
    let open = !has_viewer(&g.viewers, &call.session_id);
    drop(g);
    app.bump();
    Json(json!({ "request_id": rid, "open": open, "path": format!("/#/s/{}", call.session_id) })).into_response()
}

impl Inner {
    /// Claude moved on without the page: you answered in the terminal, or a
    /// new request replaced this one. Release the waiting hook (it exits
    /// without output) and clear the request.
    fn settle_elsewhere(&mut self, sid: &str) -> bool {
        let Some(s) = self.store.sessions.get_mut(sid) else { return false };
        let Some(p) = s.pending.take() else { return false };
        self.decisions.insert(p.id(), Decision::Terminal);
        self.waiting.remove(&p.id());
        if s.status == Status::Review {
            s.status = Status::Drafting;
        }
        s.touch();
        true
    }
}

fn has_viewer(viewers: &HashMap<String, Instant>, id: &str) -> bool {
    viewers.get(id).is_some_and(|t| t.elapsed() < VIEWER)
}

/// Runs when a hook's long-poll ends without a decision: a timeout (the hook
/// polls again at once) or a dropped connection (the hook is gone, for
/// example because you answered in the terminal). Shortly after, a request
/// nobody polls any more is cleared, so the page stops showing it as waiting.
struct PollEnd {
    app: Arc<App>,
    rid: u64,
    decided: bool,
}

const REPOLL_GRACE: Duration = Duration::from_secs(3);

impl Drop for PollEnd {
    fn drop(&mut self) {
        let (app, rid, decided) = (self.app.clone(), self.rid, self.decided);
        tokio::spawn(async move {
            {
                let mut g = app.store.lock().await;
                if let Some(n) = g.polls.get_mut(&rid) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        g.polls.remove(&rid);
                    }
                }
            }
            if decided {
                return;
            }
            // Cleared only if the hook has not polled again by then.
            tokio::time::sleep(REPOLL_GRACE).await;
            app.drop_idle(Some(rid), REPOLL_GRACE).await;
        });
    }
}

async fn hook_wait(State(app): State<Arc<App>>, headers: HeaderMap, Path(rid): Path<u64>) -> Response {
    guard!(app, headers);
    let mut rx = app.version.subscribe();
    let deadline = tokio::time::Instant::now() + POLL;
    *app.store.lock().await.polls.entry(rid).or_default() += 1;
    let mut end = PollEnd { app: app.clone(), rid, decided: false };
    loop {
        {
            let mut g = app.store.lock().await;
            if let Some(d) = g.decisions.remove(&rid) {
                g.waiting.remove(&rid);
                end.decided = true;
                return Json(d).into_response();
            }
            let alive = g.store.sessions.values().any(|s| s.pending.as_ref().is_some_and(|p| p.id() == rid));
            if !alive {
                end.decided = true;
                return err(StatusCode::NOT_FOUND, "no such request");
            }
            g.waiting.insert(rid, Instant::now());
        }
        if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
            return StatusCode::NO_CONTENT.into_response();
        }
    }
}

/// PostToolUse: plan file writes while drafting, and todos and activity while executing.
async fn hook_event(State(app): State<Arc<App>>, headers: HeaderMap, Json(v): Json<Value>) -> Response {
    guard!(app, headers);
    let Some(sid) = v["session_id"].as_str() else { return err(StatusCode::BAD_REQUEST, "no session") };
    let tool = v["tool_name"].as_str().unwrap_or("");
    let mut g = app.store.lock().await;
    let mut changed = false;
    let pending = g.store.sessions.get(sid).and_then(|s| s.pending.clone());
    match (tool, pending) {
        // The plan was approved in the terminal: work starts, in the mode you picked there.
        ("ExitPlanMode", Some(Pending::Review { .. })) => {
            g.settle_elsewhere(sid);
            if let Some(s) = g.store.sessions.get_mut(sid) {
                let mode = v["permission_mode"].as_str().unwrap_or("default").to_string();
                s.approved = Some(crate::store::Approved { round: s.rounds.len() as u32, mode, at: now_ms() });
                s.status = Status::Executing;
                s.todos.clear();
                s.activity.clear();
            }
            changed = true;
        }
        // Claude's question was answered in the terminal.
        ("AskUserQuestion", Some(Pending::Question { .. })) => {
            changed = g.settle_elsewhere(sid);
        }
        _ => {}
    }
    if let (Some(plan), Some(path)) = (v["plan"].as_str(), v["plan_path"].as_str()) {
        let cwd = v["cwd"].as_str().unwrap_or("");
        g.store.session(sid, cwd).plan_written(path, plan.to_string());
        changed = true;
    } else if let Some(s) = g.store.sessions.get_mut(sid)
        && s.status == Status::Executing {
            s.record_todos(tool, &v["tool_input"], &v["tool_response"]);
            if let Some(summary) = hook::activity_summary(tool, &v["tool_input"]) {
                s.record_activity(tool, summary);
            }
            changed = true;
        }
    if changed {
        let _ = g.store.save(sid);
        drop(g);
        app.bump();
    }
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct StopCall {
    session_id: String,
    #[serde(default)]
    stop_hook_active: bool,
    #[serde(default)]
    last_assistant_message: String,
}

async fn hook_stop(State(app): State<Arc<App>>, headers: HeaderMap, Json(call): Json<StopCall>) -> Response {
    guard!(app, headers);
    let mut g = app.store.lock().await;
    // The turn ended, so nothing is waiting on the page any more.
    g.settle_elsewhere(&call.session_id);
    let Some(s) = g.store.sessions.get_mut(&call.session_id) else { return Json(json!({})).into_response() };
    let mut context = None;
    if s.report_requested && s.report.is_none() {
        if let Some(items) = crate::store::parse_report(&call.last_assistant_message) {
            s.report = Some(crate::store::Report { at: now_ms(), items, raw: call.last_assistant_message.clone() });
            s.status = Status::Done;
            s.touch();
        }
    } else if !call.stop_hook_active && s.wants_report() {
        s.report_requested = true;
        s.touch();
        context = Some(hook::REPORT_REQUEST);
    }
    let _ = g.store.save(&call.session_id);
    drop(g);
    app.bump();
    Json(json!({ "context": context })).into_response()
}

#[derive(Deserialize)]
struct PromptCall {
    session_id: String,
}

async fn hook_prompt(State(app): State<Arc<App>>, headers: HeaderMap, Json(call): Json<PromptCall>) -> Response {
    guard!(app, headers);
    let mut g = app.store.lock().await;
    // A new prompt means you moved on from anything waiting on the page.
    if g.settle_elsewhere(&call.session_id) {
        app.bump();
    }
    let Some(s) = g.store.sessions.get_mut(&call.session_id) else { return Json(json!({})).into_response() };
    let Some(text) = s.queued.take() else {
        let _ = g.store.save(&call.session_id);
        return Json(json!({})).into_response();
    };
    s.touch();
    let _ = g.store.save(&call.session_id);
    drop(g);
    app.bump();
    Json(json!({ "context": format!("PlanOS review comments from the user, written in the plan page:\n\n{text}") })).into_response()
}

// ---------- pages ----------

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    v: u64,
}

/// Long-poll: wait until the state version moves past `since`.
async fn wait_version(app: &App, since: u64) -> u64 {
    let mut rx = app.version.subscribe();
    let now = *rx.borrow();
    if now != since {
        return now;
    }
    let _ = tokio::time::timeout(POLL, rx.changed()).await;
    *rx.borrow()
}

async fn sessions(State(app): State<Arc<App>>, headers: HeaderMap, Query(q): Query<Since>) -> Response {
    guard!(app, headers);
    let v = wait_version(&app, q.v).await;
    let mut g = app.store.lock().await;
    g.viewers.insert("*".into(), Instant::now());
    let mut list: Vec<Value> = g.store.sessions.values().map(summary).collect();
    list.sort_by_key(|s| std::cmp::Reverse(s["updated_at"].as_u64().unwrap_or(0)));
    Json(json!({ "version": v, "sessions": list })).into_response()
}

fn summary(s: &Session) -> Value {
    json!({
        "id": s.id,
        "title": s.title,
        "cwd": s.cwd,
        "status": s.status,
        "updated_at": s.updated_at,
        "rounds": s.rounds.len(),
        "open_comments": s.comments.iter().filter(|c| c.state != CommentState::Resolved).count(),
        "waiting": s.pending.as_ref().map(|p| match p { Pending::Review { .. } => "review", Pending::Question { .. } => "question" }),
        "todos_done": s.todos.iter().filter(|t| t.status == "completed").count(),
        "todos": s.todos.len(),
    })
}

async fn session(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Query(q): Query<Since>) -> Response {
    guard!(app, headers);
    let v = wait_version(&app, q.v).await;
    let mut g = app.store.lock().await;
    g.viewers.insert(id.clone(), Instant::now());
    let Some(s) = g.store.sessions.get(&id) else { return err(StatusCode::NOT_FOUND, "no such session") };
    Json(json!({ "version": v, "session": s, "doc": doc_view(s) })).into_response()
}

/// The document the page shows now: the latest round, or the live draft.
fn doc_view(s: &Session) -> Value {
    let Some(md) = s.current_markdown() else { return Value::Null };
    match s.rounds.last().filter(|r| r.markdown == md) {
        Some(r) => json!({
            "round": r.n,
            "draft": false,
            "markdown": r.markdown,
            "html": render::render(&r.markdown),
            "changed_lines": r.changed_lines,
            "removed": r.removed,
            "hidden": r.replies_span,
        }),
        None => json!({ "round": 0, "draft": true, "markdown": md, "html": render::render(md), "changed_lines": [], "removed": [], "hidden": null }),
    }
}

async fn round(State(app): State<Arc<App>>, headers: HeaderMap, Path((id, n)): Path<(String, u32)>) -> Response {
    guard!(app, headers);
    let g = app.store.lock().await;
    let Some(r) = g.store.sessions.get(&id).and_then(|s| s.rounds.iter().find(|r| r.n == n)) else {
        return err(StatusCode::NOT_FOUND, "no such round");
    };
    Json(json!({
        "round": r.n, "draft": false, "markdown": r.markdown, "html": render::render(&r.markdown),
        "changed_lines": r.changed_lines, "removed": r.removed, "hidden": r.replies_span,
    }))
    .into_response()
}

async fn delete_session(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    guard!(app, headers);
    let mut g = app.store.lock().await;
    if g.store.sessions.get(&id).is_some_and(|s| s.pending.is_some()) {
        return err(StatusCode::CONFLICT, "Claude is waiting on this session");
    }
    g.store.forget(&id);
    drop(g);
    app.bump();
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
struct NewComment {
    start_line: usize,
    end_line: usize,
    quote: String,
    text: String,
    #[serde(default)]
    suggestion: Option<String>,
}

async fn add_comment(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Json(c): Json<NewComment>) -> Response {
    guard!(app, headers);
    if c.text.trim().is_empty() && c.suggestion.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return err(StatusCode::BAD_REQUEST, "empty comment");
    }
    let mut g = app.store.lock().await;
    let Some(s) = g.store.sessions.get_mut(&id) else { return err(StatusCode::NOT_FOUND, "no such session") };
    let cid = s.add_comment(c.start_line, c.end_line, c.quote, c.text, c.suggestion);
    let _ = g.store.save(&id);
    drop(g);
    app.bump();
    Json(json!({ "id": cid })).into_response()
}

#[derive(Deserialize)]
struct EditComment {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    suggestion: Option<String>,
    #[serde(default)]
    state: Option<CommentState>,
    /// Some("") clears the reply draft.
    #[serde(default)]
    reply_draft: Option<String>,
}

async fn edit_comment(State(app): State<Arc<App>>, headers: HeaderMap, Path((id, cid)): Path<(String, String)>, Json(e): Json<EditComment>) -> Response {
    guard!(app, headers);
    let mut g = app.store.lock().await;
    let Some(s) = g.store.sessions.get_mut(&id) else { return err(StatusCode::NOT_FOUND, "no such session") };
    let Some(c) = s.comments.iter_mut().find(|c| c.id == cid) else { return err(StatusCode::NOT_FOUND, "no such comment") };
    if let Some(text) = e.text {
        if c.state != CommentState::Draft {
            return err(StatusCode::CONFLICT, "sent comments can't be edited; reply instead");
        }
        if let Some(m) = c.messages.first_mut() {
            m.text = text;
        }
    }
    if let Some(sug) = e.suggestion {
        c.suggestion = (!sug.trim().is_empty()).then_some(sug);
    }
    if let Some(state) = e.state
        && state != CommentState::Draft && c.state != CommentState::Draft {
            c.state = state;
        }
    if let Some(r) = e.reply_draft {
        c.reply_draft = (!r.trim().is_empty()).then_some(r);
    }
    s.touch();
    let _ = g.store.save(&id);
    drop(g);
    app.bump();
    StatusCode::NO_CONTENT.into_response()
}

async fn delete_comment(State(app): State<Arc<App>>, headers: HeaderMap, Path((id, cid)): Path<(String, String)>) -> Response {
    guard!(app, headers);
    let mut g = app.store.lock().await;
    let Some(s) = g.store.sessions.get_mut(&id) else { return err(StatusCode::NOT_FOUND, "no such session") };
    let before = s.comments.len();
    s.comments.retain(|c| !(c.id == cid && c.state == CommentState::Draft));
    if s.comments.len() == before {
        return err(StatusCode::CONFLICT, "only unsent comments can be deleted");
    }
    s.touch();
    let _ = g.store.save(&id);
    drop(g);
    app.bump();
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
enum UiDecision {
    Approve {
        #[serde(default = "default_mode")]
        mode: String,
    },
    Changes {
        #[serde(default)]
        general: String,
    },
    Answers {
        answers: serde_json::Map<String, Value>,
    },
    Terminal,
}

fn default_mode() -> String {
    "default".into()
}

async fn decide(State(app): State<Arc<App>>, headers: HeaderMap, Path(id): Path<String>, Json(d): Json<UiDecision>) -> Response {
    guard!(app, headers);
    let mut g = app.store.lock().await;
    let inner = &mut *g;
    let Some(s) = inner.store.sessions.get_mut(&id) else { return err(StatusCode::NOT_FOUND, "no such session") };
    let pending = s.pending.clone();
    let result = match (d, pending) {
        (UiDecision::Approve { mode }, Some(Pending::Review { id: rid, .. })) => {
            s.approved = Some(crate::store::Approved { round: s.rounds.len() as u32, mode: mode.clone(), at: now_ms() });
            s.status = Status::Executing;
            s.todos.clear();
            s.activity.clear();
            s.pending = None;
            inner.decisions.insert(rid, Decision::Approve { mode });
            json!({ "sent": "approve" })
        }
        (UiDecision::Changes { general }, Some(Pending::Review { id: rid, .. })) => {
            let Some(feedback) = s.send_feedback(&general) else {
                return err(StatusCode::BAD_REQUEST, "nothing to send: add a comment or a note");
            };
            s.status = Status::Revising;
            s.pending = None;
            inner.decisions.insert(rid, Decision::Changes { feedback });
            json!({ "sent": "changes" })
        }
        (UiDecision::Changes { general }, _) => {
            let Some(feedback) = s.send_feedback(&general) else {
                return err(StatusCode::BAD_REQUEST, "nothing to send: add a comment or a note");
            };
            s.queued = Some(match s.queued.take() {
                Some(prev) => format!("{prev}\n{feedback}"),
                None => feedback,
            });
            json!({ "sent": "queued" })
        }
        (UiDecision::Answers { answers }, Some(Pending::Question { id: rid, .. })) => {
            s.pending = None;
            inner.decisions.insert(rid, Decision::Answers { answers });
            json!({ "sent": "answers" })
        }
        (UiDecision::Terminal, Some(p)) => {
            if s.status == Status::Review {
                s.status = Status::Drafting;
            }
            s.pending = None;
            inner.decisions.insert(p.id(), Decision::Terminal);
            json!({ "sent": "terminal" })
        }
        _ => return err(StatusCode::CONFLICT, "Claude is not waiting for that"),
    };
    s.touch();
    let _ = inner.store.save(&id);
    drop(g);
    app.bump();
    Json(result).into_response()
}

// ---------- static ----------

async fn asset(path: String) -> Response {
    let Some(file) = Ui::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mut res = file.data.into_owned().into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_str(mime.as_ref()).unwrap());
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    let cache = if path.starts_with("vendor/") { "max-age=86400" } else { "no-store" };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app() -> (Arc<App>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("planos-daemon-{}-{}", std::process::id(), now_ms()));
        (App::new(&dir, "tok".into()).unwrap(), dir)
    }

    async fn call(app: &Arc<App>, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .header(TOKEN_HEADER, "tok")
            .body(Body::from(if body.is_null() { String::new() } else { body.to_string() }))
            .unwrap();
        let res = router(app.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    #[tokio::test]
    async fn api_needs_the_token() {
        let (app, _) = app();
        for uri in ["/api/health", "/api/sessions"] {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            assert_eq!(router(app.clone()).oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
        }
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let res = router(app.clone()).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().contains_key(header::CONTENT_SECURITY_POLICY));
    }

    #[tokio::test]
    async fn review_round_trip_with_comments() {
        let (app, dir) = app();
        let (st, v) = call(&app, "POST", "/api/hook/review", json!({
            "session_id": "s1", "cwd": "/p", "tool_input": {"plan": "# P\n\nStep one.\n"},
            "plan": "# P\n\nStep one.\n", "plan_path": "/plans/p.md"
        })).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(v["open"], true);
        let rid = v["request_id"].as_u64().unwrap();

        // Nothing decided yet.
        let (_, s) = call(&app, "GET", "/api/s/s1?v=0", Value::Null).await;
        assert_eq!(s["session"]["status"], "review");
        assert_eq!(s["doc"]["round"], 1);

        // A page is watching now, so the next review does not open a tab.
        let (st, _) = call(&app, "POST", "/api/s/s1/comments", json!({"start_line": 3, "end_line": 3, "quote": "Step one.", "text": "Too vague"})).await;
        assert_eq!(st, StatusCode::OK);
        let (st, _) = call(&app, "POST", "/api/s/s1/decide", json!({"decision": "changes", "general": ""})).await;
        assert_eq!(st, StatusCode::OK);

        let (st, d) = call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(d["decision"], "changes");
        assert!(d["feedback"].as_str().unwrap().contains("[c1] Line 3"));

        let (_, v) = call(&app, "POST", "/api/hook/review", json!({
            "session_id": "s1", "cwd": "/p", "tool_input": {}, "plan": "# P\n\nStep one, in detail.\n\n## Review replies\n- c1: expanded\n"
        })).await;
        assert_eq!(v["open"], false, "the page is already open");
        let rid2 = v["request_id"].as_u64().unwrap();
        let (_, s) = call(&app, "GET", "/api/s/s1?v=0", Value::Null).await;
        assert_eq!(s["session"]["comments"][0]["messages"][1]["text"], "expanded");
        assert_eq!(s["doc"]["hidden"], json!([5, 6]));

        let (_, _) = call(&app, "POST", "/api/s/s1/decide", json!({"decision": "approve", "mode": "acceptEdits"})).await;
        let (_, d) = call(&app, "GET", &format!("/api/hook/wait/{rid2}"), Value::Null).await;
        assert_eq!(d, json!({"decision": "approve", "mode": "acceptEdits"}));

        // Executing: tool events become todos and activity.
        call(&app, "POST", "/api/hook/event", json!({"session_id": "s1", "tool_name": "Edit", "tool_input": {"file_path": "/p/a.rs"}})).await;
        call(&app, "POST", "/api/hook/event", json!({"session_id": "s1", "tool_name": "TodoWrite", "tool_input": {"todos": [{"content": "a", "status": "completed"}]}})).await;
        let (_, s) = call(&app, "GET", "/api/s/s1?v=0", Value::Null).await;
        assert_eq!(s["session"]["status"], "executing");
        assert_eq!(s["session"]["activity"][0]["summary"], "/p/a.rs");

        // Stop asks for a plan check once, then reads it.
        let (_, r) = call(&app, "POST", "/api/hook/stop", json!({"session_id": "s1", "last_assistant_message": "done"})).await;
        assert!(r["context"].as_str().unwrap().contains("## Plan check"));
        let (_, r) = call(&app, "POST", "/api/hook/stop", json!({"session_id": "s1", "stop_hook_active": true, "last_assistant_message": "## Plan check\n- [done] Step one: ok\n"})).await;
        assert!(r["context"].is_null());
        let (_, s) = call(&app, "GET", "/api/s/s1?v=0", Value::Null).await;
        assert_eq!(s["session"]["status"], "done");
        assert_eq!(s["session"]["report"]["items"][0]["status"], "done");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn comments_sent_without_a_review_go_with_the_next_prompt() {
        let (app, dir) = app();
        call(&app, "POST", "/api/hook/event", json!({"session_id": "s2", "cwd": "/p", "tool_name": "Write", "plan": "# Draft\n\nIdea.\n", "plan_path": "/plans/d.md"})).await;
        let (_, s) = call(&app, "GET", "/api/s/s2?v=0", Value::Null).await;
        assert_eq!(s["doc"]["draft"], true);
        call(&app, "POST", "/api/s/s2/comments", json!({"start_line": 3, "end_line": 3, "quote": "Idea.", "text": "Which idea?"})).await;
        let (_, r) = call(&app, "POST", "/api/s/s2/decide", json!({"decision": "changes"})).await;
        assert_eq!(r["sent"], "queued");
        let (_, p) = call(&app, "POST", "/api/hook/prompt", json!({"session_id": "s2"})).await;
        assert!(p["context"].as_str().unwrap().contains("Which idea?"));
        let (_, p) = call(&app, "POST", "/api/hook/prompt", json!({"session_id": "s2"})).await;
        assert!(p["context"].is_null(), "delivered once");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn questions_are_answered_from_the_page() {
        let (app, dir) = app();
        let (_, v) = call(&app, "POST", "/api/hook/ask", json!({"session_id": "s3", "cwd": "/p", "tool_input": {"questions": [{"question": "DB?"}]}})).await;
        let rid = v["request_id"].as_u64().unwrap();
        let (st, _) = call(&app, "POST", "/api/s/s3/decide", json!({"decision": "approve"})).await;
        assert_eq!(st, StatusCode::CONFLICT, "a question can't be approved");
        call(&app, "POST", "/api/s/s3/decide", json!({"decision": "answers", "answers": {"DB?": "Postgres"}})).await;
        let (_, d) = call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await;
        assert_eq!(d["answers"]["DB?"], "Postgres");
        let (st, _) = call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_review_whose_hook_disconnects_is_cleared() {
        let (app, dir) = app();
        let (_, v) = call(&app, "POST", "/api/hook/review", json!({"session_id": "s4", "cwd": "/p", "tool_input": {}, "plan": "# P\n"})).await;
        let rid = v["request_id"].as_u64().unwrap();
        // The hook's long-poll is in flight, then its process dies.
        let waiting = {
            let app = app.clone();
            tokio::spawn(async move { call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        waiting.abort();
        let (_, s) = call(&app, "GET", "/api/s/s4?v=0", Value::Null).await;
        assert_eq!(s["session"]["status"], "review", "still waiting right after the drop");
        tokio::time::sleep(REPOLL_GRACE + Duration::from_millis(600)).await;
        let (_, s) = call(&app, "GET", "/api/s/s4?v=0", Value::Null).await;
        assert!(s["session"]["pending"].is_null());
        assert_eq!(s["session"]["status"], "drafting");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_hook_that_polls_again_keeps_its_review() {
        let (app, dir) = app();
        let (_, v) = call(&app, "POST", "/api/hook/review", json!({"session_id": "s5", "cwd": "/p", "tool_input": {}, "plan": "# P\n"})).await;
        let rid = v["request_id"].as_u64().unwrap();
        let poll = |app: Arc<App>| tokio::spawn(async move { call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await });
        // One poll ends (a timeout) while the hook's next poll is in flight,
        // and nothing changes for longer than the grace period.
        let first = poll(app.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
        let second = poll(app.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
        first.abort();
        tokio::time::sleep(REPOLL_GRACE + Duration::from_millis(600)).await;
        let (_, s) = call(&app, "GET", "/api/s/s5?v=0", Value::Null).await;
        assert_eq!(s["session"]["status"], "review", "a hook that is still polling keeps its review");
        second.abort();
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn answering_in_the_terminal_releases_the_hook() {
        let (app, dir) = app();
        // Approved in the terminal: PostToolUse on ExitPlanMode arrives.
        let (_, v) = call(&app, "POST", "/api/hook/review", json!({"session_id": "t1", "cwd": "/p", "tool_input": {}, "plan": "# P\n"})).await;
        let rid = v["request_id"].as_u64().unwrap();
        call(&app, "POST", "/api/hook/event", json!({"session_id": "t1", "tool_name": "ExitPlanMode", "permission_mode": "acceptEdits", "tool_input": {}})).await;
        let (_, d) = call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await;
        assert_eq!(d["decision"], "terminal", "the waiting hook exits quietly");
        let (_, s) = call(&app, "GET", "/api/s/t1?v=0", Value::Null).await;
        assert_eq!(s["session"]["status"], "executing");
        assert_eq!(s["session"]["approved"]["mode"], "acceptEdits");

        // A question answered in the terminal.
        let (_, v) = call(&app, "POST", "/api/hook/ask", json!({"session_id": "t2", "cwd": "/p", "tool_input": {"questions": []}})).await;
        let rid = v["request_id"].as_u64().unwrap();
        call(&app, "POST", "/api/hook/event", json!({"session_id": "t2", "tool_name": "AskUserQuestion", "tool_input": {}})).await;
        let (_, d) = call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await;
        assert_eq!(d["decision"], "terminal");

        // Dismissed, then a new prompt: the review is cleared.
        let (_, v) = call(&app, "POST", "/api/hook/review", json!({"session_id": "t3", "cwd": "/p", "tool_input": {}, "plan": "# P\n"})).await;
        let rid = v["request_id"].as_u64().unwrap();
        call(&app, "POST", "/api/hook/prompt", json!({"session_id": "t3"})).await;
        let (_, d) = call(&app, "GET", &format!("/api/hook/wait/{rid}"), Value::Null).await;
        assert_eq!(d["decision"], "terminal");
        let (_, s) = call(&app, "GET", "/api/s/t3?v=0", Value::Null).await;
        assert!(s["session"]["pending"].is_null());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn assets_are_served() {
        let (app, _) = app();
        for uri in ["/assets/app.js", "/assets/app.css", "/vendor/mermaid.min.js"] {
            let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
            assert_eq!(router(app.clone()).oneshot(req).await.unwrap().status(), StatusCode::OK, "{uri}");
        }
    }
}
