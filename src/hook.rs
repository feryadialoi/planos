//! What PlanOS writes back to Claude Code for each hook event, and the
//! review text Claude reads.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::Comment;

/// The browser's answer to a waiting hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Approve the plan and switch to `mode` (default, acceptEdits, auto).
    Approve { mode: String },
    /// Send review feedback; Claude revises.
    Changes { feedback: String },
    /// Answers to AskUserQuestion, keyed by question text.
    Answers { answers: serde_json::Map<String, Value> },
    /// Leave it to Claude Code's own terminal dialog.
    Terminal,
}

pub const MODES: [&str; 3] = ["default", "acceptEdits", "auto"];

/// Output for the ExitPlanMode PermissionRequest hook.
pub fn review_output(d: &Decision, tool_input: &Value) -> Option<Value> {
    match d {
        // ExitPlanMode needs updatedInput with allow; setMode picks the mode
        // Claude works in after approval.
        Decision::Approve { mode } => {
            let mode = if MODES.contains(&mode.as_str()) { mode.as_str() } else { "default" };
            Some(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedInput": tool_input,
                        "updatedPermissions": [{ "type": "setMode", "mode": mode, "destination": "session" }],
                    }
                }
            }))
        }
        Decision::Changes { feedback } => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PermissionRequest",
                "decision": { "behavior": "deny", "message": feedback },
            }
        })),
        Decision::Answers { .. } | Decision::Terminal => None,
    }
}

/// Output for the AskUserQuestion PreToolUse hook.
pub fn ask_output(d: &Decision, tool_input: &Value) -> Option<Value> {
    let Decision::Answers { answers } = d else { return None };
    let mut input = tool_input.clone();
    input["answers"] = Value::Object(answers.clone());
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "permissionDecisionReason": "Answered in PlanOS",
            "updatedInput": input,
        }
    }))
}

/// Context added to a hook without blocking (Stop, UserPromptSubmit).
pub fn context_output(event: &str, text: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": event, "additionalContext": text } })
}

pub const REPORT_REQUEST: &str = "PlanOS: the approved plan looks finished. Before you stop, end your reply with a \
\"## Plan check\" section: one line per step of the approved plan, written as \
\"- [done] step: note\", using done, changed, skipped or partial. Be honest about anything not done.";

/// The review as Claude reads it.
pub fn feedback(new: &[Comment], follow: &[(Comment, String)], general: &str, plan_path: Option<&str>) -> String {
    let mut head = vec![];
    match new.len() {
        0 => {}
        1 => head.push("1 comment".to_string()),
        n => head.push(format!("{n} comments")),
    }
    match follow.len() {
        0 => {}
        1 => head.push("1 follow-up".to_string()),
        n => head.push(format!("{n} follow-ups")),
    }
    let mut out = if head.is_empty() {
        "Plan review: changes requested.".to_string()
    } else {
        format!("Plan review: {}.", head.join(", "))
    };
    out.push_str(" Revise the plan and call ExitPlanMode again.\n");
    if let Some(p) = plan_path.filter(|_| !new.is_empty()) {
        out.push_str(&format!("Line numbers refer to {p}.\n"));
    }

    for c in new {
        let lines = match (c.start_line, c.end_line) {
            (0, _) => String::new(),
            (s, e) if e <= s => format!("Line {s}, "),
            (s, e) => format!("Lines {s}-{e}, "),
        };
        let what = if c.suggestion.is_some() { "suggested edit on" } else { "on" };
        out.push_str(&format!("\n[{}] {lines}{what} \"{}\"\n", c.id, short_quote(&c.quote)));
        if let Some(first) = c.messages.first() {
            quote_lines(&mut out, &first.text);
        }
        if let Some(s) = &c.suggestion {
            out.push_str(&format!("    Replace with: {}\n", s.trim()));
        }
    }
    for (c, reply) in follow {
        out.push_str(&format!("\n[{}] Follow-up on \"{}\"\n", c.id, short_quote(&c.quote)));
        quote_lines(&mut out, reply);
    }

    if !general.is_empty() {
        out.push_str(&format!("\nGeneral: {general}\n"));
    }
    let ids: Vec<&str> = new.iter().map(|c| c.id.as_str()).chain(follow.iter().map(|(c, _)| c.id.as_str())).collect();
    if !ids.is_empty() {
        out.push_str(&format!(
            "\nEnd the revised plan with a \"## Review replies\" section, one line per comment ({}), \
             written as \"- {}: what you changed, or why you did not\".\n",
            ids.join(", "),
            ids[0]
        ));
    }
    out
}

fn quote_lines(out: &mut String, text: &str) {
    for line in text.trim().lines() {
        out.push_str(&format!("    > {line}\n"));
    }
}

fn short_quote(q: &str) -> String {
    const MAX: usize = 120;
    let flat = q.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    let cut: String = flat.chars().take(MAX).collect();
    format!("{}...", cut.trim_end())
}

/// One-line summary of a tool call for the activity feed.
pub fn activity_summary(tool: &str, input: &Value) -> Option<String> {
    let s = |k: &str| input[k].as_str().map(str::to_string);
    let text = match tool {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" | "Read" => s("file_path").or(s("notebook_path"))?,
        "Bash" => s("description").or(s("command"))?,
        "Grep" | "Glob" => s("pattern")?,
        "WebFetch" => s("url")?,
        "WebSearch" => s("query")?,
        "Agent" | "Task" => s("description")?,
        _ => return None,
    };
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if flat.chars().count() > 140 { format!("{}...", flat.chars().take(140).collect::<String>()) } else { flat })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Author, CommentState, Message};

    fn c(id: &str, s: usize, e: usize, quote: &str, text: &str) -> Comment {
        Comment {
            id: id.into(),
            state: CommentState::Open,
            start_line: s,
            end_line: e,
            quote: quote.into(),
            suggestion: None,
            messages: vec![Message { author: Author::You, text: text.into(), round: 1 }],
            reply_draft: None,
            outdated: false,
        }
    }

    #[test]
    fn feedback_lists_comments_with_ids_and_asks_for_replies() {
        let got = feedback(
            &[c("c1", 12, 14, "migrate all tables\nin one transaction", "Split by table.\nLocks prod."), c("c2", 30, 30, "skip tests", "No.")],
            &[],
            "keep the rollback section",
            Some("/p/plan.md"),
        );
        assert_eq!(
            got,
            "Plan review: 2 comments. Revise the plan and call ExitPlanMode again.\n\
             Line numbers refer to /p/plan.md.\n\
             \n[c1] Lines 12-14, on \"migrate all tables in one transaction\"\n    > Split by table.\n    > Locks prod.\n\
             \n[c2] Line 30, on \"skip tests\"\n    > No.\n\
             \nGeneral: keep the rollback section\n\
             \nEnd the revised plan with a \"## Review replies\" section, one line per comment (c1, c2), \
             written as \"- c1: what you changed, or why you did not\".\n"
        );
    }

    #[test]
    fn general_only_feedback_has_no_reply_instruction() {
        let got = feedback(&[], &[], "too big, split it", Some("/p/plan.md"));
        assert_eq!(got, "Plan review: changes requested. Revise the plan and call ExitPlanMode again.\n\nGeneral: too big, split it\n");
    }

    #[test]
    fn approve_sets_mode_and_echoes_input() {
        let input = json!({"plan": "# P", "planFilePath": "/p.md"});
        let out = review_output(&Decision::Approve { mode: "acceptEdits".into() }, &input).unwrap();
        let d = &out["hookSpecificOutput"]["decision"];
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PermissionRequest");
        assert_eq!(d["behavior"], "allow");
        assert_eq!(d["updatedInput"], input);
        assert_eq!(d["updatedPermissions"][0], json!({"type": "setMode", "mode": "acceptEdits", "destination": "session"}));
        let out = review_output(&Decision::Approve { mode: "bypassPermissions".into() }, &input).unwrap();
        assert_eq!(out["hookSpecificOutput"]["decision"]["updatedPermissions"][0]["mode"], "default");
    }

    #[test]
    fn changes_deny_and_terminal_prints_nothing() {
        let out = review_output(&Decision::Changes { feedback: "fix it".into() }, &json!({})).unwrap();
        assert_eq!(out["hookSpecificOutput"]["decision"], json!({"behavior": "deny", "message": "fix it"}));
        assert!(review_output(&Decision::Terminal, &json!({})).is_none());
    }

    #[test]
    fn answers_go_into_updated_input() {
        let input = json!({"questions": [{"question": "Which DB?"}]});
        let mut answers = serde_json::Map::new();
        answers.insert("Which DB?".into(), json!("Postgres"));
        let out = ask_output(&Decision::Answers { answers }, &input).unwrap();
        let h = &out["hookSpecificOutput"];
        assert_eq!(h["permissionDecision"], "allow");
        assert_eq!(h["updatedInput"]["answers"]["Which DB?"], "Postgres");
        assert_eq!(h["updatedInput"]["questions"], input["questions"]);
        assert!(ask_output(&Decision::Terminal, &input).is_none());
    }

    #[test]
    fn decisions_parse_from_ui_json() {
        let d: Decision = serde_json::from_str(r#"{"decision":"approve","mode":"auto"}"#).unwrap();
        assert_eq!(d, Decision::Approve { mode: "auto".into() });
        let d: Decision = serde_json::from_str(r#"{"decision":"terminal"}"#).unwrap();
        assert_eq!(d, Decision::Terminal);
    }

    #[test]
    fn activity_summaries() {
        assert_eq!(activity_summary("Edit", &json!({"file_path": "/a/b.rs"})).as_deref(), Some("/a/b.rs"));
        assert_eq!(activity_summary("Bash", &json!({"command": "ls", "description": "List files"})).as_deref(), Some("List files"));
        assert!(activity_summary("TodoWrite", &json!({})).is_none());
    }
}
