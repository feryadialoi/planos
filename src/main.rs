mod hook;
mod render;
mod server;

use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context, Result};

const USAGE: &str = "PlanOS: review Claude Code plans in the browser.

Usage:
  planos hook      Run as a Claude Code PreToolUse hook for ExitPlanMode
                   (reads the hook JSON on stdin, prints the decision on stdout)
  planos --version

Environment:
  PLANOS_NO_BROWSER=1   Print the review URL instead of opening a browser
  PLANOS_URL_FILE=path  Also write the review URL to this file";

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("hook") => match run_hook() {
            Ok(()) => ExitCode::SUCCESS,
            // Exit 1 is a non-blocking hook error: Claude Code shows its own
            // plan dialog, so a PlanOS failure never strands the session.
            Err(e) => {
                eprintln!("PlanOS: {e:#}");
                ExitCode::from(1)
            }
        },
        Some("--version" | "-V") => {
            println!("planos {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run_hook() -> Result<()> {
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin).context("read hook input")?;
    let input: hook::HookInput = serde_json::from_str(&stdin).context("parse hook input")?;

    let Some((markdown, path)) = input.plan() else {
        eprintln!("PlanOS: no plan in the hook input, leaving it to Claude Code");
        return Ok(());
    };

    let plan = server::Plan {
        title: render::title(&markdown).unwrap_or_else(|| "Plan".into()),
        path,
        html: render::render(&markdown),
        markdown,
    };
    let (review, rx) = server::Review::new(plan, server::token()?);
    let open = std::env::var_os("PLANOS_NO_BROWSER").is_none();

    let rt = tokio::runtime::Runtime::new().context("start runtime")?;
    let submission = rt.block_on(server::run(review, rx, open))?;

    if let Some(out) = hook::output(&submission, &input.tool_input) {
        println!("{out}");
    }
    Ok(())
}
