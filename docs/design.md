# PlanOS design: end-to-end planning

PlanOS follows a Claude Code plan from first draft to a checked result:

| Stage | What PlanOS does | Claude Code hook |
|---|---|---|
| Shape | Shows the plan live while Claude writes it; answers Claude's clarifying questions in a form | `PostToolUse` (async) on writes to `~/.claude/plans/`; `PreToolUse` on `AskUserQuestion` in plan mode |
| Review | Rounds with a diff to the previous round; comment threads that carry across rounds; suggested edits; mermaid and code highlighting | `PermissionRequest` on `ExitPlanMode` |
| Approve | Approve with a permission mode: manual, accept edits, or auto | `PermissionRequest` allow with `updatedInput` and `setMode` |
| Execute | Live checklist from Claude's todos and an activity feed | `PostToolUse` (async) |
| Verify | A plan check: each step done, changed, skipped or partial | `Stop` with `additionalContext` |
| Across | One page listing every session, with history; nothing is lost on reload | all of the above, through one local service |

Comments written while Claude is not waiting for a review are delivered with
your next message (`UserPromptSubmit` with `additionalContext`).

## Shape of the system

```
Claude Code hooks ──> planos hook (short-lived client)
                          │  HTTP, token in a header
                          ▼
                     planos serve (local service, 127.0.0.1:47820)
                          │  ~/.planos/sessions/<id>.json
                          ▼
                     browser page (dashboard and session views)
```

- `planos hook` reads the hook JSON, dispatches on `hook_event_name`, and
  starts the service when it is not running (or runs an older version).
- Blocking hooks (review, questions) long-poll the service for the decision.
  A review whose hook stops polling for 60 seconds is dropped, so a review
  answered in the terminal does not stay "waiting" in the page.
- Every failure in a blocking hook exits 1, a non-blocking error, so Claude
  Code falls back to its own dialog.
- The browser opens only when no PlanOS tab is already watching that session.

## Review rounds and threads

- Each `ExitPlanMode` call is a round. The service diffs it against the
  previous round (line diff) and the page can show what changed.
- Comments are drafts until sent. Sending gives each an id (`c1`, `c2`, ...).
- The deny message asks Claude to end the revised plan with a
  `## Review replies` section, one line per comment (`- c1: what changed`).
  The service attaches those lines to the threads and the page hides the
  section from the document.
- Open comments are re-anchored in the new round by their quoted text. One
  whose text is gone is marked outdated but kept.
- You resolve a thread, or reply; replies go out with the next round.

## Verified Claude Code behavior (2.1.325)

- `PermissionRequest` fires for `ExitPlanMode` in plan mode. Allow with
  `updatedInput` and `updatedPermissions: [{type: setMode}]` approves the plan
  ("User approved Claude's plan") and switches the mode. Deny with `message`
  sends the message to Claude, which revises.
- In an interactive session Claude Code shows its own plan dialog while the
  `PermissionRequest` hook runs; whichever answers first wins. PlanOS treats
  the terminal as a second way to answer, not a failure.
- `PreToolUse` allow with `updatedInput.answers` answers `AskUserQuestion`.
- `Stop` with `additionalContext` makes Claude write the plan check; Claude
  followed the `## Plan check` format in an end-to-end run.
- Claude followed the `## Review replies` instruction in an end-to-end run.
- `ExitPlanMode` is not offered with `claude -p`, so tests drive an
  interactive session.

Not yet seen in a real session: Claude making a task list (`TodoWrite` or the
Task tools), so the live checklist is covered by unit tests only.
