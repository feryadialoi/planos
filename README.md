# PlanOS

Plan with Claude Code in the browser, from the first draft to a checked result.

| Stage | What you get |
|---|---|
| Shape | The plan shows up live while Claude writes it. Claude's clarifying questions open as a form. |
| Review | Select text and press `C` to comment, or `S` to suggest new wording. Each round shows what changed since your last comments. Threads carry across rounds: Claude answers each comment, and you resolve it or reply. Mermaid diagrams and code are rendered. |
| Approve | Approve with the mode Claude works in next: manual, accept edits, or auto. |
| Execute | Claude's task list and every file it touches, live. |
| Verify | When the work looks finished, Claude writes a plan check: each step done, changed, skipped or partial. |

Every session is listed on one page, and nothing is lost on reload. Comments you
write while Claude is not waiting for a review go out with your next message.

Claude Code's own dialogs stay available: whichever you answer first, the
terminal or the page, wins.

## Install

Needs [Rust](https://rustup.rs) 1.85 or newer and Claude Code.

**One line, no clone:**

```sh
curl -fsSL https://raw.githubusercontent.com/feryadialoi/planos/main/install.sh | bash
```

**From a clone:**

```sh
git clone https://github.com/feryadialoi/planos && cd planos
./install.sh
```

The script builds `planos` into `~/.cargo/bin`, adds the `planos` marketplace
to Claude Code and installs the `planos@planos` plugin. Run it again to
upgrade.

**By hand**, if you prefer:

```sh
cargo install --locked --git https://github.com/feryadialoi/planos
claude plugin marketplace add feryadialoi/planos
claude plugin install planos@planos
```

Then restart Claude Code (or run `/reload-plugins`), enter plan mode
(`shift+tab`) and ask for a plan. `planos open` opens the page any time.

**Uninstall:**

```sh
./install.sh --uninstall     # or: curl -fsSL .../install.sh | bash -s -- --uninstall
```

This removes the plugin, the marketplace and the binary. Saved plans stay in
`~/.planos`; delete that folder to remove them too.

## How it works

```
Claude Code hooks ──> planos hook ──HTTP──> planos serve (127.0.0.1:47820) <── browser
```

- `planos hook` handles every hook and starts the local service when needed
  (or restarts it after an upgrade). State lives in `~/.planos/sessions/`.
- Hooks: `PermissionRequest` on `ExitPlanMode` (review and approve),
  `PreToolUse` on `AskUserQuestion` in plan mode (questions), `PostToolUse`,
  async (live draft, tasks, activity), `Stop` (plan check) and
  `UserPromptSubmit` (comments queued for your next message).
- If PlanOS fails, the hook exits 1, a non-blocking error, and Claude Code
  carries on with its own dialog.
- Safety: plans are rendered in Rust with raw HTML escaped, the page runs under
  a strict Content Security Policy, and every API call needs the token in
  `~/.planos/token` (mode 0600), sent in a header.

See [docs/design.md](docs/design.md) for the design.

## Configuration

| Setting | Effect |
|---|---|
| `~/.planos/config.json` `{"browser": [...]}` | Command that opens the page, for example Chrome with `--profile-directory=Default` |
| `PLANOS_HOME` | State directory (default `~/.planos`) |
| `PLANOS_PORT` | Service port (default 47820) |
| `PLANOS_NO_BROWSER=1` | Print the page URL instead of opening a browser |
| `PLANOS_URL_FILE=path` | Also write the page URL to this file |

## Develop

```sh
cargo test
```

In debug builds, `rust-embed` reads `ui/` from disk, so page edits show on
reload. Release builds embed `ui/` into the binary. Mermaid is vendored under
`ui/vendor/` (MIT, see `mermaid.LICENSE`).
