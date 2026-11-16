//! Sessions, review rounds, comment threads and execution tracking.
//! Pure state: no I/O except saving each session as JSON.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Claude is writing the plan.
    Drafting,
    /// Claude presented the plan and waits for a decision.
    Review,
    /// Changes were requested; Claude is revising.
    Revising,
    /// The plan was approved and Claude is working.
    Executing,
    /// The plan check came back.
    Done,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Round {
    pub n: u32,
    pub markdown: String,
    pub at: u64,
    /// New-side lines (1-based) inserted or changed since the previous round.
    #[serde(default)]
    pub changed_lines: Vec<usize>,
    /// Text removed since the previous round, placed before `before_line`.
    #[serde(default)]
    pub removed: Vec<Removed>,
    /// Line span of the `## Review replies` section, hidden in the page.
    #[serde(default)]
    pub replies_span: Option<(usize, usize)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Removed {
    pub before_line: usize,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentState {
    /// Written in the page, not sent to Claude yet.
    Draft,
    /// Sent; waiting for Claude's answer or for you to resolve it.
    Open,
    Resolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Author {
    You,
    Claude,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub author: Author,
    pub text: String,
    pub round: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub state: CommentState,
    /// Lines in the current document (re-anchored each round).
    pub start_line: usize,
    pub end_line: usize,
    pub quote: String,
    /// Replacement text when this is a suggested edit.
    #[serde(default)]
    pub suggestion: Option<String>,
    pub messages: Vec<Message>,
    /// Your reply waiting to go out with the next round.
    #[serde(default)]
    pub reply_draft: Option<String>,
    /// The quoted text is no longer in the plan.
    #[serde(default)]
    pub outdated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Pending {
    Review { id: u64, tool_input: Value, since: u64 },
    Question { id: u64, tool_input: Value, since: u64 },
}

impl Pending {
    pub fn id(&self) -> u64 {
        match self {
            Pending::Review { id, .. } | Pending::Question { id, .. } => *id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub id: String,
    pub text: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub at: u64,
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckItem {
    pub status: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub at: u64,
    pub items: Vec<CheckItem>,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approved {
    pub round: u32,
    pub mode: String,
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub plan_path: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    pub status: Status,
    /// Plan file contents while Claude writes it, before it is presented.
    #[serde(default)]
    pub draft: Option<String>,
    #[serde(default)]
    pub rounds: Vec<Round>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub next_comment: u32,
    #[serde(default)]
    pub pending: Option<Pending>,
    #[serde(default)]
    pub approved: Option<Approved>,
    #[serde(default)]
    pub todos: Vec<Todo>,
    #[serde(default)]
    pub activity: Vec<Activity>,
    #[serde(default)]
    pub report_requested: bool,
    #[serde(default)]
    pub report: Option<Report>,
    /// Feedback sent while Claude was not waiting; goes with your next message.
    #[serde(default)]
    pub queued: Option<String>,
}

const ACTIVITY_CAP: usize = 300;

impl Session {
    pub fn new(id: &str, cwd: &str) -> Self {
        let now = now_ms();
        Self {
            id: id.into(),
            cwd: cwd.into(),
            title: "Untitled plan".into(),
            plan_path: None,
            created_at: now,
            updated_at: now,
            status: Status::Drafting,
            draft: None,
            rounds: vec![],
            comments: vec![],
            next_comment: 0,
            pending: None,
            approved: None,
            todos: vec![],
            activity: vec![],
            report_requested: false,
            report: None,
            queued: None,
        }
    }

    /// The document the page shows and comments anchor to: the live draft
    /// while it is newer than the last presented round.
    pub fn current_markdown(&self) -> Option<&str> {
        match (&self.draft, self.rounds.last()) {
            (Some(d), Some(r)) if self.status == Status::Drafting || self.status == Status::Revising => {
                if d != &r.markdown { Some(d) } else { Some(&r.markdown) }
            }
            (Some(d), None) => Some(d),
            (_, Some(r)) => Some(&r.markdown),
            (None, None) => None,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = now_ms();
    }

    /// Claude wrote the plan file.
    pub fn plan_written(&mut self, path: &str, markdown: String) {
        self.plan_path = Some(path.into());
        if let Some(t) = crate::render::title(&markdown) {
            self.title = t;
        }
        self.draft = Some(markdown);
        if matches!(self.status, Status::Executing | Status::Done) {
            // A new plan after an approved one starts a new cycle.
            self.status = Status::Drafting;
            self.approved = None;
            self.report = None;
            self.report_requested = false;
            self.todos.clear();
        }
        self.reanchor_current();
        self.touch();
    }

    /// Claude called ExitPlanMode: a new round.
    pub fn present(&mut self, markdown: String, path: Option<String>, pending_id: u64, tool_input: Value) {
        if path.is_some() {
            self.plan_path = path;
        }
        if let Some(t) = crate::render::title(&markdown) {
            self.title = t;
        }
        if matches!(self.status, Status::Executing | Status::Done) {
            self.approved = None;
            self.report = None;
            self.report_requested = false;
            self.todos.clear();
        }
        let n = self.rounds.len() as u32 + 1;
        let (changed_lines, removed) = match self.rounds.last() {
            Some(prev) => diff(&prev.markdown, &markdown),
            None => (vec![], vec![]),
        };
        let replies = parse_replies(&markdown);
        for (id, text) in &replies.lines {
            if let Some(c) = self.comments.iter_mut().find(|c| &c.id == id) {
                c.messages.push(Message { author: Author::Claude, text: text.clone(), round: n });
            }
        }
        self.rounds.push(Round { n, markdown: markdown.clone(), at: now_ms(), changed_lines, removed, replies_span: replies.span });
        self.draft = Some(markdown);
        self.reanchor_current();
        self.status = Status::Review;
        self.pending = Some(Pending::Review { id: pending_id, tool_input, since: now_ms() });
        self.touch();
    }

    fn reanchor_current(&mut self) {
        let Some(md) = self.current_markdown().map(str::to_string) else { return };
        let hidden = self.rounds.last().and_then(|r| r.replies_span);
        for c in self.comments.iter_mut().filter(|c| c.state != CommentState::Resolved) {
            match find_quote(&md, &c.quote) {
                Some((s, e)) if hidden.is_none_or(|(a, b)| s < a || s > b) => {
                    c.start_line = s;
                    c.end_line = e;
                    c.outdated = false;
                }
                _ => c.outdated = true,
            }
        }
    }

    pub fn add_comment(&mut self, start_line: usize, end_line: usize, quote: String, text: String, suggestion: Option<String>) -> String {
        self.next_comment += 1;
        let id = format!("c{}", self.next_comment);
        let round = self.rounds.len() as u32;
        self.comments.push(Comment {
            id: id.clone(),
            state: CommentState::Draft,
            start_line,
            end_line,
            quote,
            suggestion,
            messages: vec![Message { author: Author::You, text, round }],
            reply_draft: None,
            outdated: false,
        });
        self.touch();
        id
    }

    /// Mark drafts and reply drafts as sent and build the text Claude reads.
    /// Returns `None` when there is nothing to send.
    pub fn send_feedback(&mut self, general: &str) -> Option<String> {
        let round = self.rounds.len() as u32;
        let mut new = vec![];
        let mut follow = vec![];
        for c in self.comments.iter_mut() {
            if c.state == CommentState::Draft {
                c.state = CommentState::Open;
                new.push(c.clone());
            } else if let Some(reply) = c.reply_draft.take().filter(|r| !r.trim().is_empty()) {
                c.messages.push(Message { author: Author::You, text: reply.clone(), round });
                c.state = CommentState::Open;
                follow.push((c.clone(), reply));
            }
        }
        let general = general.trim();
        if new.is_empty() && follow.is_empty() && general.is_empty() {
            return None;
        }
        self.touch();
        Some(crate::hook::feedback(&new, &follow, general, self.plan_path.as_deref()))
    }

    pub fn record_activity(&mut self, tool: &str, summary: String) {
        self.activity.push(Activity { at: now_ms(), tool: tool.into(), summary });
        if self.activity.len() > ACTIVITY_CAP {
            let extra = self.activity.len() - ACTIVITY_CAP;
            self.activity.drain(..extra);
        }
        self.touch();
    }

    /// Claude's todo list, from TodoWrite or the Task tools.
    pub fn record_todos(&mut self, tool: &str, input: &Value, response: &Value) {
        match tool {
            "TodoWrite" => {
                if let Some(list) = input["todos"].as_array() {
                    self.todos = list
                        .iter()
                        .enumerate()
                        .map(|(i, t)| Todo {
                            id: t["id"].as_str().map(str::to_string).unwrap_or_else(|| (i + 1).to_string()),
                            text: t["content"].as_str().or(t["subject"].as_str()).unwrap_or("").to_string(),
                            status: t["status"].as_str().unwrap_or("pending").to_string(),
                        })
                        .collect();
                }
            }
            "TaskCreate" => {
                let id = first_str(&[&response["task"]["id"], &response["taskId"], &response["id"], &input["taskId"]])
                    .unwrap_or_else(|| (self.todos.len() + 1).to_string());
                let text = first_str(&[&input["subject"], &input["content"], &input["description"]]).unwrap_or_default();
                self.todos.push(Todo { id, text, status: "pending".into() });
            }
            "TaskUpdate" => {
                let Some(id) = first_str(&[&input["taskId"], &input["id"]]) else { return };
                if let Some(t) = self.todos.iter_mut().find(|t| t.id == id) {
                    if let Some(s) = input["status"].as_str() {
                        t.status = s.to_string();
                    }
                    if let Some(s) = input["subject"].as_str() {
                        t.text = s.to_string();
                    }
                }
            }
            _ => return,
        }
        self.touch();
    }

    /// The Stop hook asks for a plan check once the approved work looks done.
    pub fn wants_report(&self) -> bool {
        self.status == Status::Executing
            && !self.report_requested
            && self.report.is_none()
            && (self.todos.is_empty() || self.todos.iter().all(|t| t.status == "completed" || t.status == "deleted"))
    }
}

fn first_str(values: &[&Value]) -> Option<String> {
    values.iter().find_map(|v| match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

/// Line diff from `old` to `new`: changed new-side lines and removed text.
pub fn diff(old: &str, new: &str) -> (Vec<usize>, Vec<Removed>) {
    use similar::{DiffTag, TextDiff};
    let d = TextDiff::from_lines(old, new);
    let old_lines: Vec<&str> = old.lines().collect();
    let mut changed = vec![];
    let mut removed = vec![];
    for op in d.ops() {
        let (tag, o, n) = op.as_tag_tuple();
        match tag {
            DiffTag::Equal => {}
            DiffTag::Insert | DiffTag::Replace | DiffTag::Delete => {
                changed.extend(n.clone().map(|i| i + 1));
                let gone: Vec<&str> = o.clone().filter_map(|i| old_lines.get(i).copied()).collect();
                let text = gone.join("\n");
                if (tag == DiffTag::Delete || tag == DiffTag::Replace) && !text.trim().is_empty() {
                    removed.push(Removed { before_line: n.start + 1, text });
                }
            }
        }
    }
    (changed, removed)
}

pub struct Replies {
    pub lines: Vec<(String, String)>,
    pub span: Option<(usize, usize)>,
}

/// The `## Review replies` section Claude adds to a revised plan:
/// one `- c1: text` line per comment.
pub fn parse_replies(md: &str) -> Replies {
    let lines: Vec<&str> = md.lines().collect();
    let Some(start) = lines.iter().position(|l| {
        let t = l.trim();
        t.starts_with('#') && t.trim_start_matches('#').trim().eq_ignore_ascii_case("review replies")
    }) else {
        return Replies { lines: vec![], span: None };
    };
    let level = lines[start].trim().chars().take_while(|c| *c == '#').count();
    let mut end = lines.len();
    for (i, l) in lines.iter().enumerate().skip(start + 1) {
        let t = l.trim();
        let hashes = t.chars().take_while(|c| *c == '#').count();
        if hashes > 0 && hashes <= level && t.chars().nth(hashes) == Some(' ') {
            end = i;
            break;
        }
    }
    let mut out = vec![];
    for l in &lines[start + 1..end] {
        let t = l.trim().trim_start_matches(['-', '*']).trim();
        let t = t.trim_start_matches('[');
        let id_len = t.chars().take_while(|c| *c == 'c' || c.is_ascii_digit()).count();
        if id_len < 2 || !t.starts_with('c') {
            continue;
        }
        let id = &t[..id_len];
        let rest = t[id_len..].trim_start_matches(']').trim_start_matches([':', ' ', '-']).trim();
        if !rest.is_empty() {
            out.push((id.to_string(), rest.to_string()));
        }
    }
    // Trailing blank lines belong to nothing; keep the span tight.
    let mut last = end;
    while last > start + 1 && lines[last - 1].trim().is_empty() {
        last -= 1;
    }
    Replies { lines: out, span: Some((start + 1, last)) }
}

/// Find quoted (rendered) text in markdown source; returns its 1-based line
/// span. Markdown punctuation and spacing are ignored on both sides.
pub fn find_quote(md: &str, quote: &str) -> Option<(usize, usize)> {
    let needle = normalize(quote);
    if needle.is_empty() {
        return None;
    }
    let mut hay = String::new();
    let mut line_of = vec![]; // line number for each char of `hay`
    for (i, line) in md.lines().enumerate() {
        let n = normalize(strip_markers(line));
        if n.is_empty() {
            continue;
        }
        if !hay.is_empty() {
            hay.push(' ');
            line_of.push(i + 1);
        }
        for ch in n.chars() {
            hay.push(ch);
            line_of.push(i + 1);
        }
    }
    let hay_chars: Vec<char> = hay.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let pos = find_chars(&hay_chars, &needle_chars).or_else(|| {
        // A long quote that was lightly edited still anchors by its opening.
        (needle_chars.len() > 40).then(|| find_chars(&hay_chars, &needle_chars[..40])).flatten()
    })?;
    let end = (pos + needle_chars.len()).min(hay_chars.len()) - 1;
    Some((line_of[pos], line_of[end]))
}

fn find_chars(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()] == *needle)
}

fn strip_markers(line: &str) -> &str {
    let t = line.trim_start();
    let t = t.trim_start_matches('#').trim_start_matches('>').trim_start();
    let t = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("- [x] ")).unwrap_or(t);
    let t = t.trim_start_matches(['-', '*', '+']).trim_start();
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 && t[digits..].starts_with(['.', ')']) {
        return t[digits + 1..].trim_start();
    }
    t
}

fn normalize(s: &str) -> String {
    let kept: String = s
        .chars()
        .filter(|c| !matches!(c, '*' | '_' | '`' | '|' | '[' | ']'))
        .collect();
    kept.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// `## Plan check` from Claude's final message.
pub fn parse_report(text: &str) -> Option<Vec<CheckItem>> {
    let mut lines = text.lines().skip_while(|l| {
        let t = l.trim();
        !(t.starts_with('#') && t.trim_start_matches('#').trim().eq_ignore_ascii_case("plan check"))
    });
    lines.next()?;
    let mut items = vec![];
    for l in lines {
        let t = l.trim();
        if t.starts_with('#') {
            break;
        }
        let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) else { continue };
        let rest = rest.trim();
        let (status, text) = match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((s, t)) => (s.trim().to_lowercase(), t.trim().to_string()),
            None => match rest.split_once(':') {
                Some((s, t)) if ["done", "changed", "skipped", "partial"].contains(&s.trim().to_lowercase().as_str()) => {
                    (s.trim().to_lowercase(), t.trim().to_string())
                }
                _ => ("done".to_string(), rest.to_string()),
            },
        };
        items.push(CheckItem { status, text });
    }
    (!items.is_empty()).then_some(items)
}

// ---------- persistence ----------

pub struct Store {
    dir: PathBuf,
    pub sessions: HashMap<String, Session>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Self> {
        let dir = dir.join("sessions");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let mut sessions = HashMap::new();
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| Ok(serde_json::from_slice::<Session>(&b)?)) {
                Ok(mut s) => {
                    // A review whose hook died with the old service is not waiting any more.
                    if s.pending.take().is_some() && s.status == Status::Review {
                        s.status = Status::Drafting;
                    }
                    sessions.insert(s.id.clone(), s);
                }
                Err(e) => eprintln!("PlanOS: skipping {}: {e:#}", path.display()),
            }
        }
        Ok(Self { dir, sessions })
    }

    pub fn session(&mut self, id: &str, cwd: &str) -> &mut Session {
        self.sessions.entry(id.to_string()).or_insert_with(|| Session::new(id, cwd))
    }

    pub fn forget(&mut self, id: &str) {
        if self.sessions.remove(id).is_some() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            let _ = std::fs::remove_file(self.dir.join(format!("{id}.json")));
        }
    }

    pub fn save(&self, id: &str) -> Result<()> {
        let Some(s) = self.sessions.get(id) else { return Ok(()) };
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            anyhow::bail!("bad session id");
        }
        let tmp = self.dir.join(format!("{id}.json.tmp"));
        std::fs::write(&tmp, serde_json::to_vec(s)?)?;
        std::fs::rename(&tmp, self.dir.join(format!("{id}.json")))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const V1: &str = "# Plan: rate limit\n\n## Steps\n1. Add middleware.\n2. Migrate all tables in one transaction.\n3. Skip tests for now.\n";
    const V2: &str = "# Plan: rate limit\n\n## Steps\n1. Add middleware.\n2. Migrate each table in its own transaction.\n3. Add integration tests.\n4. Roll out behind a flag.\n\n## Review replies\n- c1: split into one migration per table\n- [c2] added an integration test step\n";

    #[test]
    fn diff_marks_changed_lines_and_removed_text() {
        let (changed, removed) = diff(V1, V2);
        assert!(changed.contains(&5) && changed.contains(&6) && changed.contains(&7));
        assert!(!changed.contains(&4));
        assert_eq!(removed.len(), 1);
        assert!(removed[0].text.contains("Migrate all tables"));
        assert_eq!(removed[0].before_line, 5);
    }

    #[test]
    fn replies_are_parsed_with_their_span() {
        let r = parse_replies(V2);
        assert_eq!(
            r.lines,
            vec![
                ("c1".to_string(), "split into one migration per table".to_string()),
                ("c2".to_string(), "added an integration test step".to_string())
            ]
        );
        // Heading on line 9 through the last reply on line 11.
        assert_eq!(r.span, Some((9, 11)));
        assert!(parse_replies(V1).span.is_none());
    }

    #[test]
    fn replies_section_ends_at_next_heading() {
        let md = "# P\n## Review replies\n- c1: ok\n\n## Next\ntext\n";
        let r = parse_replies(md);
        assert_eq!(r.lines.len(), 1);
        assert_eq!(r.span, Some((2, 3)));
    }

    #[test]
    fn quotes_anchor_through_markdown_formatting() {
        let md = "# T\n\n1. Add a `RateLimiter` middleware\n   backed by **Redis**.\n2. Other.\n";
        assert_eq!(find_quote(md, "Add a RateLimiter middleware backed by Redis"), Some((3, 4)));
        assert_eq!(find_quote(md, "Other."), Some((5, 5)));
        assert_eq!(find_quote(md, "not here"), None);
    }

    #[test]
    fn full_review_cycle_threads_and_reanchors() {
        let mut s = Session::new("s1", "/p");
        s.present(V1.into(), Some("/plans/p.md".into()), 1, json!({}));
        assert_eq!(s.status, Status::Review);
        assert_eq!(s.title, "Plan: rate limit");

        let c1 = s.add_comment(5, 5, "Migrate all tables in one transaction".into(), "Split by table.".into(), None);
        let c2 = s.add_comment(6, 6, "Skip tests for now".into(), "No.".into(), Some("Add integration tests.".into()));
        assert_eq!((c1.as_str(), c2.as_str()), ("c1", "c2"));

        let fb = s.send_feedback("keep rollback").unwrap();
        assert!(fb.contains("[c1] Line 5"));
        assert!(fb.contains("[c2] Line 6, suggested edit"));
        assert!(fb.contains("Replace with: Add integration tests."));
        assert!(fb.contains("## Review replies"));
        assert!(s.comments.iter().all(|c| c.state == CommentState::Open));
        assert!(s.send_feedback("").is_none(), "nothing new to send");

        s.status = Status::Revising;
        s.present(V2.into(), None, 2, json!({}));
        assert_eq!(s.rounds.len(), 2);
        let c1 = &s.comments[0];
        assert_eq!(c1.messages.last().unwrap().author, Author::Claude);
        assert_eq!(c1.messages.last().unwrap().text, "split into one migration per table");
        // "Migrate all tables in one transaction" is gone from round 2.
        assert!(c1.outdated);
        assert_eq!(s.comments[1].messages.last().unwrap().text, "added an integration test step");

        // A reply to Claude goes out as a follow-up.
        s.comments[0].reply_draft = Some("Also split by tenant.".into());
        let fb = s.send_feedback("").unwrap();
        assert!(fb.contains("[c1] Follow-up"));
        assert!(fb.contains("> Also split by tenant."));
    }

    #[test]
    fn todos_from_todowrite_and_task_tools() {
        let mut s = Session::new("s", "/");
        s.record_todos("TodoWrite", &json!({"todos": [{"content": "a", "status": "completed"}, {"content": "b", "status": "in_progress"}]}), &json!({}));
        assert_eq!(s.todos.len(), 2);
        assert_eq!(s.todos[1].status, "in_progress");

        let mut t = Session::new("t", "/");
        t.record_todos("TaskCreate", &json!({"subject": "write tests"}), &json!({"task": {"id": "7"}}));
        t.record_todos("TaskUpdate", &json!({"taskId": "7", "status": "completed"}), &json!({}));
        assert_eq!(t.todos, vec![Todo { id: "7".into(), text: "write tests".into(), status: "completed".into() }]);
    }

    #[test]
    fn report_is_requested_once_work_looks_done() {
        let mut s = Session::new("s", "/");
        s.status = Status::Executing;
        s.todos = vec![Todo { id: "1".into(), text: "a".into(), status: "in_progress".into() }];
        assert!(!s.wants_report());
        s.todos[0].status = "completed".into();
        assert!(s.wants_report());
        s.report_requested = true;
        assert!(!s.wants_report());
    }

    #[test]
    fn plan_check_parses_statuses() {
        let text = "Done.\n\n## Plan check\n- [done] Add middleware: shipped\n- [skipped] Load test: no staging\n- changed: Migration split per table\n- plain line\n";
        let items = parse_report(text).unwrap();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0], CheckItem { status: "done".into(), text: "Add middleware: shipped".into() });
        assert_eq!(items[1].status, "skipped");
        assert_eq!(items[2], CheckItem { status: "changed".into(), text: "Migration split per table".into() });
        assert_eq!(items[3].status, "done");
        assert!(parse_report("no report here").is_none());
    }

    #[test]
    fn store_round_trips_and_drops_stale_reviews() {
        let dir = std::env::temp_dir().join(format!("planos-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir).unwrap();
        store.session("abc", "/p").present(V1.into(), None, 9, json!({}));
        store.save("abc").unwrap();
        let reopened = Store::open(&dir).unwrap();
        let s = &reopened.sessions["abc"];
        assert_eq!(s.rounds.len(), 1);
        assert!(s.pending.is_none());
        assert_eq!(s.status, Status::Drafting);
        assert!(store.save("../evil").is_ok(), "unknown ids are ignored");
        store.sessions.insert("../evil".into(), Session::new("../evil", "/"));
        assert!(store.save("../evil").is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
