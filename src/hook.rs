//! Claude Code hook I/O: the PreToolUse input for ExitPlanMode, and the
//! decision PlanOS writes back.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
pub struct HookInput {
    #[serde(default)]
    pub tool_input: Value,
}

impl HookInput {
    /// Plan markdown and its file path. Claude Code injects `plan`; the file is
    /// the fallback when it is missing.
    pub fn plan(&self) -> Option<(String, Option<String>)> {
        let path = self.tool_input["planFilePath"].as_str().map(str::to_string);
        let text = match self.tool_input["plan"].as_str() {
            Some(p) if !p.trim().is_empty() => p.to_string(),
            _ => std::fs::read_to_string(path.as_deref()?).ok()?,
        };
        Some((text, path))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Comment {
    pub start_line: usize,
    pub end_line: usize,
    pub quote: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Submission {
    Approve,
    Changes {
        #[serde(default)]
        comments: Vec<Comment>,
        #[serde(default)]
        general: String,
    },
    /// Hand the decision back to Claude Code's own terminal dialog.
    Terminal,
}

/// The JSON to print on stdout, or `None` to print nothing so Claude Code
/// falls back to its normal plan dialog.
pub fn output(sub: &Submission, tool_input: &Value) -> Option<Value> {
    match sub {
        // ExitPlanMode only skips the prompt when `allow` carries updatedInput.
        Submission::Approve => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "Plan approved in PlanOS",
                "updatedInput": tool_input,
            }
        })),
        Submission::Changes { comments, general } => {
            let path = tool_input["planFilePath"].as_str();
            Some(json!({
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": feedback(comments, general, path),
                }
            }))
        }
        Submission::Terminal => None,
    }
}

/// The review as Claude reads it.
pub fn feedback(comments: &[Comment], general: &str, plan_path: Option<&str>) -> String {
    let mut out = match comments.len() {
        0 => "Plan review: changes requested.".to_string(),
        1 => "Plan review: 1 comment.".to_string(),
        n => format!("Plan review: {n} comments."),
    };
    out.push_str(" Revise the plan and call ExitPlanMode again.\n");
    if let Some(p) = plan_path.filter(|_| !comments.is_empty()) {
        out.push_str(&format!("Line numbers refer to {p}.\n"));
    }

    for (i, c) in comments.iter().enumerate() {
        let lines = match (c.start_line, c.end_line) {
            (0, _) => String::new(),
            (s, e) if e <= s => format!("Line {s}, "),
            (s, e) => format!("Lines {s}-{e}, "),
        };
        out.push_str(&format!("\n[{}] {lines}on \"{}\"\n", i + 1, short_quote(&c.quote)));
        for line in c.text.trim().lines() {
            out.push_str(&format!("    > {line}\n"));
        }
    }

    let general = general.trim();
    if !general.is_empty() {
        out.push_str(&format!("\nGeneral: {general}\n"));
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: usize, e: usize, quote: &str, text: &str) -> Comment {
        Comment { start_line: s, end_line: e, quote: quote.into(), text: text.into() }
    }

    #[test]
    fn feedback_lists_each_comment_with_lines_and_quote() {
        let got = feedback(
            &[
                c(12, 14, "migrate all tables\nin one transaction", "Split by table.\nLocks prod."),
                c(30, 30, "skip tests for now", "No. Add an integration test step."),
            ],
            "keep the rollback section",
            Some("/p/plan.md"),
        );
        assert_eq!(
            got,
            "Plan review: 2 comments. Revise the plan and call ExitPlanMode again.\n\
             Line numbers refer to /p/plan.md.\n\
             \n[1] Lines 12-14, on \"migrate all tables in one transaction\"\n    > Split by table.\n    > Locks prod.\n\
             \n[2] Line 30, on \"skip tests for now\"\n    > No. Add an integration test step.\n\
             \nGeneral: keep the rollback section\n"
        );
    }

    #[test]
    fn general_only_feedback() {
        let got = feedback(&[], "too big, split it", Some("/p/plan.md"));
        assert_eq!(
            got,
            "Plan review: changes requested. Revise the plan and call ExitPlanMode again.\n\
             \nGeneral: too big, split it\n"
        );
    }

    #[test]
    fn long_quotes_are_shortened() {
        let q = "word ".repeat(60);
        let got = short_quote(&q);
        assert!(got.ends_with("..."));
        assert!(got.chars().count() <= 123);
    }

    #[test]
    fn approve_echoes_tool_input_as_updated_input() {
        let input = json!({"plan": "# P", "planFilePath": "/p.md"});
        let out = output(&Submission::Approve, &input).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        assert_eq!(out["hookSpecificOutput"]["updatedInput"], input);
    }

    #[test]
    fn changes_deny_with_feedback_and_terminal_prints_nothing() {
        let input = json!({"plan": "# P", "planFilePath": "/p.md"});
        let sub = Submission::Changes { comments: vec![c(1, 1, "P", "rename")], general: String::new() };
        let out = output(&sub, &input).unwrap();
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(out["hookSpecificOutput"]["permissionDecisionReason"].as_str().unwrap().contains("> rename"));
        assert!(output(&Submission::Terminal, &input).is_none());
    }

    #[test]
    fn submission_parses_from_ui_json() {
        let s: Submission = serde_json::from_str(
            r#"{"decision":"changes","comments":[{"start_line":2,"end_line":3,"quote":"q","text":"t"}]}"#,
        )
        .unwrap();
        assert_eq!(s, Submission::Changes { comments: vec![c(2, 3, "q", "t")], general: String::new() });
        let s: Submission = serde_json::from_str(r#"{"decision":"approve"}"#).unwrap();
        assert_eq!(s, Submission::Approve);
    }

    #[test]
    fn plan_falls_back_to_file() {
        let dir = std::env::temp_dir().join(format!("planos-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("plan.md");
        std::fs::write(&f, "# From file").unwrap();
        let input = HookInput { tool_input: json!({"planFilePath": f.to_str().unwrap()}) };
        assert_eq!(input.plan().unwrap().0, "# From file");
        let empty = HookInput { tool_input: json!({}) };
        assert!(empty.plan().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
