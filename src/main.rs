mod client;
mod daemon;
mod hook;
mod render;
mod store;

use std::io::Read;
use std::process::ExitCode;

use anyhow::{Context, Result};

const USAGE: &str = "PlanOS: plan with Claude Code in the browser.

Usage:
  planos hook      Run as a Claude Code hook (reads the hook JSON on stdin)
  planos open      Open the PlanOS page
  planos serve     Run the local service (started for you by the hooks)
  planos status    Show whether the service is running
  planos stop      Stop the service
  planos --version

Environment:
  PLANOS_HOME=dir       State directory (default ~/.planos)
  PLANOS_PORT=n         Service port (default 47820)
  PLANOS_NO_BROWSER=1   Print the page URL instead of opening a browser, and never move focus
  PLANOS_URL_FILE=path  Also write the page URL to this file

~/.planos/config.json can set the browser:
  {\"browser\": [\"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome\", \"--profile-directory=Default\"]}
and turn off moving between the terminal and the page:
  {\"focus\": false}";

fn main() -> ExitCode {
    let result = match std::env::args().nth(1).as_deref() {
        Some("hook") => run_hook(),
        Some("serve") => serve(),
        Some("open") => open(),
        Some("status") => status(),
        Some("stop") => stop(),
        Some("--version" | "-V") => {
            println!("planos {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        // For hooks, exit 1 is a non-blocking error: Claude Code carries on
        // with its own dialog, so a PlanOS failure never strands a session.
        Err(e) => {
            eprintln!("PlanOS: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn run_hook() -> Result<()> {
    let mut stdin = String::new();
    std::io::stdin().read_to_string(&mut stdin).context("read hook input")?;
    let input: serde_json::Value = serde_json::from_str(&stdin).context("parse hook input")?;
    if let Some(out) = client::run_hook(&input)? {
        println!("{out}");
    }
    Ok(())
}

fn serve() -> Result<()> {
    let home = client::home();
    let token = client::token(&home)?;
    let rt = tokio::runtime::Runtime::new().context("start runtime")?;
    rt.block_on(daemon::serve(home, client::port(), token))
}

fn open() -> Result<()> {
    let c = client::Client::new()?;
    c.ensure()?;
    c.open_browser(&c.url("/"));
    Ok(())
}

fn status() -> Result<()> {
    let c = client::Client::new()?;
    match c.version() {
        Some(v) => println!("PlanOS service {v} running at {}", c.base),
        None => println!("PlanOS service is not running"),
    }
    Ok(())
}

fn stop() -> Result<()> {
    let c = client::Client::new()?;
    if c.version().is_none() {
        println!("PlanOS service is not running");
        return Ok(());
    }
    let res = ureq::post(&format!("{}/api/shutdown", c.base)).set(daemon::TOKEN_HEADER, &c.token).call();
    match res {
        Ok(_) => println!("PlanOS service stopped"),
        Err(e) => anyhow::bail!("could not stop the service: {e}"),
    }
    Ok(())
}
