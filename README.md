# PlanOS

Review Claude Code plans in the browser instead of the terminal.

When Claude finishes a plan in plan mode and calls `ExitPlanMode`, PlanOS opens the
plan as a rendered document. Select any text (or use the `+` beside a block) to leave
a comment, then:

- **Request changes**: your comments go back to Claude, which revises the plan and
  opens a new review.
- **Approve**: Claude leaves plan mode and starts the work.
- **Answer in terminal**: closes the review and shows Claude Code's normal plan dialog.

Claude receives the comments in this form:

```
Plan review: 2 comments. Revise the plan and call ExitPlanMode again.
Line numbers refer to /Users/you/.claude/plans/rate-limit.md.

[1] Lines 12-14, on "migrate all tables in one transaction"
    > Split by table. One transaction locks prod too long.

[2] Line 30, on "skip tests for now"
    > No. Add an integration test step.

General: keep the rollback section.
```

## Install

```sh
cargo install --path .          # puts `planos` in ~/.cargo/bin
```

Then add the plugin in Claude Code:

```
/plugin marketplace add /path/to/planos
/plugin install planos@planos
```

To try it for one session without installing the plugin:

```sh
claude --plugin-dir /path/to/planos
```

## How it works

The plugin registers a `PreToolUse` hook on `ExitPlanMode` that runs `planos hook`.
The hook reads the plan from the hook input, serves it on `127.0.0.1` at a random
port, opens the browser, and blocks until you submit.

| Your choice | Hook output |
|---|---|
| Approve | `permissionDecision: "allow"` with `updatedInput` (required for ExitPlanMode to skip its dialog) |
| Request changes | `permissionDecision: "deny"` with the comments as the reason |
| Answer in terminal | nothing, so Claude Code shows its own dialog |

If PlanOS fails, it exits with code 1 (a non-blocking hook error) and Claude Code
falls back to its own dialog.

Safety: markdown is rendered in Rust with raw HTML escaped, the page runs under a
strict Content Security Policy, and every API call needs a per-review random token
sent in a custom header.

## Environment

| Variable | Effect |
|---|---|
| `PLANOS_NO_BROWSER=1` | Print the review URL to stderr instead of opening a browser |
| `PLANOS_URL_FILE=path` | Also write the review URL to this file |

## Develop

```sh
cargo test
```

In debug builds, `rust-embed` reads `ui/` from disk, so UI edits show on reload.
Release builds embed `ui/` into the binary.
