//! The short-lived side of PlanOS: what each Claude Code hook runs. It talks
//! to the local service, starting it (or replacing an older version) first.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use crate::daemon::TOKEN_HEADER;
use crate::hook::{self, Decision};

pub const DEFAULT_PORT: u16 = 47820;

pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("PLANOS_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join(".planos")
}

pub fn port() -> u16 {
    std::env::var("PLANOS_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(DEFAULT_PORT)
}

/// The shared secret between hooks, the service and the page. Created once,
/// readable only by you.
pub fn token(home: &std::path::Path) -> Result<String> {
    let path = home.join("token");
    if let Ok(t) = std::fs::read_to_string(&path)
        && !t.trim().is_empty() {
            return Ok(t.trim().to_string());
        }
    std::fs::create_dir_all(home).with_context(|| format!("create {}", home.display()))?;
    let mut buf = [0u8; 24];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("random token: {e}"))?;
    let t: String = buf.iter().map(|b| format!("{b:02x}")).collect();
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    match opts.open(&path) {
        Ok(mut f) => {
            std::io::Write::write_all(&mut f, t.as_bytes())?;
            Ok(t)
        }
        // Another hook created it first.
        Err(_) => Ok(std::fs::read_to_string(&path)?.trim().to_string()),
    }
}

pub struct Client {
    pub base: String,
    pub token: String,
    pub home: PathBuf,
}

impl Client {
    pub fn new() -> Result<Self> {
        let home = home();
        Ok(Self { base: format!("http://127.0.0.1:{}", port()), token: token(&home)?, home })
    }

    fn agent(&self, read: Duration) -> ureq::Agent {
        ureq::AgentBuilder::new().timeout_connect(Duration::from_millis(500)).timeout_read(read).build()
    }

    fn get(&self, path: &str, read: Duration) -> Result<(u16, Value)> {
        let res = self.agent(read).get(&format!("{}{path}", self.base)).set(TOKEN_HEADER, &self.token).call();
        read_response(res)
    }

    fn post(&self, path: &str, body: &Value, read: Duration) -> Result<(u16, Value)> {
        let res = self.agent(read).post(&format!("{}{path}", self.base)).set(TOKEN_HEADER, &self.token).send_json(body);
        read_response(res)
    }

    pub fn version(&self) -> Option<String> {
        let (code, v) = self.get("/api/health", Duration::from_secs(2)).ok()?;
        (code == 200).then(|| v["version"].as_str().unwrap_or("").to_string())
    }

    /// Make sure this version of the service is running.
    pub fn ensure(&self) -> Result<()> {
        match self.version() {
            Some(v) if v == env!("CARGO_PKG_VERSION") => return Ok(()),
            Some(_) => {
                let _ = self.post("/api/shutdown", &json!({}), Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(400));
            }
            None => {}
        }
        self.spawn()?;
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            if self.version().as_deref() == Some(env!("CARGO_PKG_VERSION")) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("the PlanOS service did not start; see {}", self.home.join("service.log").display())
    }

    fn spawn(&self) -> Result<()> {
        let log = std::fs::OpenOptions::new().create(true).append(true).open(self.home.join("service.log"))?;
        let mut cmd = std::process::Command::new(std::env::current_exe()?);
        cmd.arg("serve").stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(log);
        // Its own process group, so it outlives the hook and Claude Code's signals to it.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        cmd.spawn().context("start the PlanOS service")?;
        Ok(())
    }

    pub fn url(&self, path: &str) -> String {
        // The token rides in the query; routes live in the hash.
        let (route, hash) = path.split_once('#').unwrap_or((path, ""));
        let route = if route.is_empty() { "/" } else { route };
        if hash.is_empty() { format!("{}{route}?t={}", self.base, self.token) } else { format!("{}{route}?t={}#{hash}", self.base, self.token) }
    }

    pub fn open_browser(&self, url: &str) {
        if let Some(f) = std::env::var_os("PLANOS_URL_FILE") {
            let _ = std::fs::write(f, url);
        }
        if std::env::var_os("PLANOS_NO_BROWSER").is_some() {
            eprintln!("PlanOS: {url}");
            return;
        }
        let mut cmd = match browser_command(&self.home) {
            Some(argv) => {
                let mut c = std::process::Command::new(&argv[0]);
                c.args(&argv[1..]);
                c
            }
            None if cfg!(target_os = "macos") => std::process::Command::new("open"),
            None if cfg!(windows) => {
                let mut c = std::process::Command::new("cmd");
                c.args(["/C", "start", ""]);
                c
            }
            None => std::process::Command::new("xdg-open"),
        };
        cmd.arg(url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
        if let Err(e) = cmd.spawn() {
            eprintln!("PlanOS: could not open a browser ({e}). Open {url}");
        }
    }

    /// Block until the page decides; None when the review went away.
    fn wait(&self, rid: u64) -> Result<Decision> {
        let mut failures = 0;
        loop {
            match self.get(&format!("/api/hook/wait/{rid}"), Duration::from_secs(40)) {
                Ok((200, v)) => {
                    if v["from_page"] == true {
                        focus_terminal(&self.home);
                    }
                    return Ok(serde_json::from_value(v)?);
                }
                Ok((204, _)) => failures = 0,
                Ok((404, _)) => bail!("the review was closed"),
                Ok((code, v)) => bail!("PlanOS service answered {code}: {v}"),
                Err(e) => {
                    failures += 1;
                    if failures > 5 {
                        return Err(e.context("lost the PlanOS service"));
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }
}

fn read_response(res: Result<ureq::Response, ureq::Error>) -> Result<(u16, Value)> {
    let res = match res {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(e) => return Err(anyhow::Error::from(e)),
    };
    let code = res.status();
    let text = res.into_string().unwrap_or_default();
    Ok((code, serde_json::from_str(&text).unwrap_or(Value::Null)))
}

/// `~/.planos/config.json`, or null when it is missing or broken.
fn config(home: &std::path::Path) -> Value {
    std::fs::read_to_string(home.join("config.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null)
}

/// `"focus": false` stops PlanOS moving you between the terminal and the page.
pub fn focus_enabled(home: &std::path::Path) -> bool {
    config(home)["focus"].as_bool().unwrap_or(true)
}

/// The terminal app Claude Code runs in, for `open`: `"terminal_app"` from
/// the config (an app name or path), else the app that launched the shell.
fn terminal_app(home: &std::path::Path) -> Option<(&'static str, String)> {
    if let Some(app) = config(home)["terminal_app"].as_str() {
        return Some(("-a", app.to_string()));
    }
    if let Ok(id) = std::env::var("__CFBundleIdentifier")
        && !id.is_empty() {
            return Some(("-b", id));
        }
    let id = match std::env::var("TERM_PROGRAM").ok()?.as_str() {
        "rex" => "com.superlogical.rex",
        "Apple_Terminal" => "com.apple.Terminal",
        "iTerm.app" => "com.googlecode.iterm2",
        "ghostty" => "com.mitchellh.ghostty",
        "WezTerm" => "com.github.wez.wezterm",
        "WarpTerminal" => "dev.warp.Warp-Stable",
        _ => return None,
    };
    Some(("-b", id.to_string()))
}

/// After you decide on the page, bring Claude Code's terminal forward, and in
/// Rex its pane too. macOS only; PLANOS_NO_BROWSER turns it off for tests.
fn focus_terminal(home: &std::path::Path) {
    if !cfg!(target_os = "macos") || std::env::var_os("PLANOS_NO_BROWSER").is_some() || !focus_enabled(home) {
        return;
    }
    let quiet = |mut c: std::process::Command| {
        c.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        let _ = c.status();
    };
    if let Ok(block) = std::env::var("REX_BLOCK") {
        let mut c = std::process::Command::new("rex");
        c.args(["focus", &block]);
        quiet(c);
    }
    if let Some((flag, app)) = terminal_app(home) {
        let mut c = std::process::Command::new("open");
        c.args([flag, &app]);
        quiet(c);
    }
}

/// `~/.planos/config.json`: {"browser": ["/path/to/browser", "--flag"]}
fn browser_command(home: &std::path::Path) -> Option<Vec<String>> {
    let v = config(home);
    let argv: Vec<String> = v["browser"].as_array()?.iter().filter_map(|a| a.as_str().map(str::to_string)).collect();
    (!argv.is_empty()).then_some(argv)
}

// ---------- hook handlers ----------

/// Runs one hook: reads its JSON, prints Claude Code's answer (or nothing).
pub fn run_hook(input: &Value) -> Result<Option<Value>> {
    let event = input["hook_event_name"].as_str().unwrap_or("");
    let tool = input["tool_name"].as_str().unwrap_or("");
    match (event, tool) {
        ("PermissionRequest", "ExitPlanMode") => review(input),
        ("PreToolUse", "AskUserQuestion") if input["permission_mode"] == "plan" => ask(input),
        ("PostToolUse", _) => {
            tool_event(input);
            Ok(None)
        }
        ("Stop", _) => Ok(quick("/api/hook/stop", input, "Stop")),
        ("UserPromptSubmit", _) => Ok(quick("/api/hook/prompt", input, "UserPromptSubmit")),
        _ => Ok(None),
    }
}

fn plan_text(tool_input: &Value) -> Option<(String, Option<String>)> {
    let path = tool_input["planFilePath"].as_str().map(str::to_string);
    let text = match tool_input["plan"].as_str() {
        Some(p) if !p.trim().is_empty() => p.to_string(),
        _ => std::fs::read_to_string(path.as_deref()?).ok()?,
    };
    Some((text, path))
}

fn review(input: &Value) -> Result<Option<Value>> {
    let Some((plan, plan_path)) = plan_text(&input["tool_input"]) else {
        eprintln!("PlanOS: no plan in the hook input, leaving it to Claude Code");
        return Ok(None);
    };
    let c = Client::new()?;
    c.ensure()?;
    let body = json!({
        "session_id": input["session_id"], "cwd": input["cwd"], "tool_input": input["tool_input"],
        "plan": plan, "plan_path": plan_path,
    });
    let decision = c.start_and_wait("/api/hook/review", &body)?;
    Ok(hook::review_output(&decision, &input["tool_input"]))
}

fn ask(input: &Value) -> Result<Option<Value>> {
    let c = Client::new()?;
    c.ensure()?;
    let body = json!({ "session_id": input["session_id"], "cwd": input["cwd"], "tool_input": input["tool_input"] });
    let decision = c.start_and_wait("/api/hook/ask", &body)?;
    Ok(hook::ask_output(&decision, &input["tool_input"]))
}

impl Client {
    fn start_and_wait(&self, path: &str, body: &Value) -> Result<Decision> {
        let (code, v) = self.post(path, body, Duration::from_secs(10))?;
        if code != 200 {
            bail!("PlanOS service answered {code}: {v}");
        }
        let rid = v["request_id"].as_u64().context("no request id")?;
        if v["open"] == true {
            self.open_browser(&self.url(v["path"].as_str().unwrap_or("/")));
        } else if let Some(f) = std::env::var_os("PLANOS_URL_FILE") {
            let _ = std::fs::write(f, self.url(v["path"].as_str().unwrap_or("/")));
        }
        self.wait(rid)
    }
}

const PLAN_TOOLS: [&str; 3] = ["Write", "Edit", "MultiEdit"];
const TODO_TOOLS: [&str; 3] = ["TodoWrite", "TaskCreate", "TaskUpdate"];

/// PostToolUse (async): plan file writes start or update a session; other
/// calls only matter to a session that is executing, which the service decides.
fn tool_event(input: &Value) {
    let Ok(c) = Client::new() else { return };
    let tool = input["tool_name"].as_str().unwrap_or("");
    let path = input["tool_input"]["file_path"].as_str().unwrap_or("");
    let mut body = json!({
        "session_id": input["session_id"], "cwd": input["cwd"], "tool_name": tool,
        "permission_mode": input["permission_mode"], "tool_input": slim(&input["tool_input"]),
    });
    if TODO_TOOLS.contains(&tool) {
        body["tool_input"] = input["tool_input"].clone();
        body["tool_response"] = input["tool_response"].clone();
    }
    if PLAN_TOOLS.contains(&tool) && path.contains("/.claude/plans/") && path.ends_with(".md")
        && let Ok(text) = std::fs::read_to_string(path) {
            body["plan"] = Value::String(text);
            body["plan_path"] = Value::String(path.to_string());
            if c.ensure().is_err() {
                return;
            }
        }
    let _ = c.post("/api/hook/event", &body, Duration::from_secs(3));
}

/// Tool inputs without bulky file contents.
fn slim(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() {
        for k in ["content", "old_string", "new_string", "edits", "new_source"] {
            o.remove(k);
        }
    }
    v
}

/// Stop and UserPromptSubmit: never start the service, never block long.
fn quick(path: &str, input: &Value, event: &str) -> Option<Value> {
    let c = Client::new().ok()?;
    let (code, v) = c.post(path, input, Duration::from_secs(3)).ok()?;
    let text = v["context"].as_str().filter(|_| code == 200)?;
    Some(hook::context_output(event, text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slim_drops_file_contents() {
        let v = slim(&json!({"file_path": "/a", "content": "x".repeat(1000), "old_string": "a", "new_string": "b"}));
        assert_eq!(v, json!({"file_path": "/a"}));
    }

    #[test]
    fn urls_keep_token_in_query_and_route_in_hash() {
        let c = Client { base: "http://127.0.0.1:1".into(), token: "t".into(), home: PathBuf::new() };
        assert_eq!(c.url("/#/s/abc"), "http://127.0.0.1:1/?t=t#/s/abc");
        assert_eq!(c.url("/"), "http://127.0.0.1:1/?t=t");
    }

    #[test]
    fn token_is_created_once_and_private() {
        let dir = std::env::temp_dir().join(format!("planos-token-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = token(&dir).unwrap();
        assert_eq!(a.len(), 48);
        assert_eq!(token(&dir).unwrap(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("token")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plan_text_falls_back_to_the_file() {
        let dir = std::env::temp_dir().join(format!("planos-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("p.md");
        std::fs::write(&f, "# From file").unwrap();
        assert_eq!(plan_text(&json!({"planFilePath": f.to_str().unwrap()})).unwrap().0, "# From file");
        assert!(plan_text(&json!({})).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn questions_outside_plan_mode_are_left_alone() {
        let out = run_hook(&json!({"hook_event_name": "PreToolUse", "tool_name": "AskUserQuestion", "permission_mode": "default"})).unwrap();
        assert!(out.is_none());
    }
}
