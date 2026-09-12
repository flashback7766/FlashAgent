# Changelog

Beta builds are numbered `bNNN` and ship on the rolling `beta` pre-release.
Stable versions start at `v1.0.0` and ship on the rolling `stable` release.

## Unreleased

- While the model works, the mascot's face sits in the status line —
  `(•_•) [Manual] · Generating response...` — blinking and winking on the
  turn's own clock. The welcome card is gone by then, so this is the only
  place it can keep you company.
- Fixed: `--resume` drew only the top border of the welcome card. The card is
  animated in row by row on start-up, and a resumed session prints its
  messages over it immediately, which stopped the animation halfway. A resumed
  session now gets the whole card at once.
- The follow-up suggestion above the input box no longer offers prompts the
  assistant meant for you ("Расскажи о своём проекте и опиши, в чём нужна
  помощь"). Pressing → sends the suggestion to the model, so a question aimed
  at you was worse than no suggestion at all; those are now dropped, and the
  analyzer is told plainly to write a message the assistant can answer on its
  own, in the language of the conversation. The giveaway is the object, not
  the verb — "расскажи об архитектуре" still passes.
- The mascot is drawn properly. A terminal cell is twice as tall as it is
  wide, so the old sprite — one pixel per cell — came out as a stretched,
  spiky kite. It now draws two pixels per cell, which makes the pixels square
  and the shape round. It also breathes (the highlight rises and falls over
  about three seconds), and the welcome card draws itself in from the top over
  half a second on start-up.
- The mascot's face reports whether the model server answered: eyes open and a
  smile when it did, eyes shut and drained colour when it did not, neutral
  while the first check is still running. Discovery reruns every few seconds,
  so starting your server turns the face around by itself — on a first run
  that is the difference between "nothing happens" and knowing why.
- `/goal` takes budgets: `/goal [--steps N] [--time 30m] [--tokens 200k]
  <task>`, defaulting to 250 steps and one hour. `--tokens` counts generated
  tokens only — with a prefix cache, a total-token budget mostly measures how
  long the conversation is. The status line shows the burn-down live
  (`step 12/250 · 4.2k tok · 3m05s/1h0m`), and the run ends with a report card
  built from loop events, not from the model's own account: steps, generated
  tokens, elapsed, files created and edited, shell commands that failed, and
  an explicit INCOMPLETE when a budget (or Esc) ended the run.
- The command is just `flashagent`. Packages, tarballs, the AppImage and the
  installers no longer add a `flashagent-tui` alias (reinstalling removes the
  old one); in-app hints use the new name. The empty `crates/app` is gone.
- External (MCP) tools run without approval only when your `.mcp.json` marks
  them `read_only` / lists them in `read_only_tools`. Tool names like `get_*`
  and the server's own `readOnlyHint` no longer skip the approval card.

## b233 — end-to-end audit

A full audit that traced every user-visible feature from key press to model,
tool and screen, then fixed what did not hold up. Every fix has a regression
test; the whole stack was also exercised live against LM Studio
(`gemma-4-e2b-it-qat@q4_k_xl`) in a real terminal.

### Tool calling and the agent loop
- Tool calls that models write as text (`<tool_call>`, `[TOOL_CALLS]`, bare
  JSON) are now executed. The parser existed but was never wired in; it only
  accepts names of advertised tools, never looks inside code fences (tracked
  across streamed chunks), runs every call of a Mistral array, and never
  "completes" a block that was cut off.
- Interrupting a turn no longer corrupts the conversation: every recorded tool
  call gets a result (`cancelled by user`) and partial answer text is kept, so
  the next request is valid and the model knows what happened.
- Servers that omit or repeat tool-call ids get unique 9-character ids (the
  shape Mistral templates require); nameless calls are dropped.
- A backend error mid-turn no longer discards the steps that already ran;
  a turn that ends without any reply is closed so strict chat templates
  (Gemma, Mistral) never see two user messages in a row.
- The stall-recovery nudge no longer leaks into history as a fake user message.
- Tool calls cut off by the output token limit are no longer executed (JSON
  repair could "complete" a truncated path or command into a different one);
  the model is told to resend them.

### LLM backend
- A persistent HTTP 400 recursed until the stack overflowed and the app
  aborted. Adaptation is now bounded: learn thinking presets only from errors
  about thinking, then fall back to fewer optional fields, then give up.
- Errors a server reports inside the stream (e.g. context overflow) surface as
  errors instead of an empty answer.
- The 10-minute hard request timeout is replaced by connect + idle timeouts, so
  long generations on slow local models are no longer cut off.
- `network_retries` now retries connection failures (it was never read);
  requests that may already have reached the server are never retried.
- Tool arguments sent as JSON objects (Ollama and older vLLM) are accepted.

### Permissions and safety
- "Always" on a shell approval card allowed every shell command for the rest
  of the session. It now adds per-segment rules: `program subcommand` with
  flags becomes a prefix rule (`cargo test ...`), anything else is allowed
  only verbatim (`rm -rf build` never grows into `rm -rf build ~`).
- Shell allow-rules never match commands containing `$(`, backticks,
  redirections, backslash escapes or `$'..'`.
- The permission check, the approval card, the diff preview and the tool now
  read arguments through one resolver, so the command that is approved is the
  command that runs (a stray `"arguments"` wrapper could make them differ).
- The approval card always shows the command, path or MCP arguments being
  approved — wrapped, never clipped, with whitespace padding made visible and
  control characters neutralised.
- MCP read-only annotations (`readOnlyHint`) and the `read_only` /
  `read_only_tools` settings in `.mcp.json` now actually skip approval cards.
- Interrupted or timed-out shell commands are killed with their whole process
  tree; a backgrounded child no longer hangs the tool call; stdin is closed.
- Subagents are cancelled together with the turn that spawned them.

### MCP
- Server-initiated requests (`roots/list`, `ping`, ...) are answered and can no
  longer be mistaken for replies to our own requests.
- The client no longer advertises capabilities it does not implement.
- `tools/list` pagination, invalid-argument errors, and a real "Error"/"Stopped"
  status instead of showing broken servers as "Disabled".
- Timed-out or cancelled requests are forgotten and the server is sent
  `notifications/cancelled`.

### TUI
- Esc / Ctrl+C interrupt cooperatively (3 s hard-abort fallback) instead of
  killing the turn and discarding its history. Ctrl+C on an approval or
  question card now denies and interrupts instead of quitting the app.
- Settings open on the live session state and only persist defaults you change
  there (the view no longer writes the config file itself); "Run Tool Test"
  runs a real tool-calling probe (it always said "passed").
- `/compact` keeps the current turn intact and folds the summary into the single
  system message (it could orphan tool results or stack system messages).
- `--resume` no longer stacks an old system prompt, keeps compaction summaries
  from older sessions, and reports a missing session.
- The context gauge counts the real tool schemas and no longer double-counts memory.
- Late recaps no longer derail the live turn's rendering.
- `/effort default` uses the server's default preset; unknown efforts are rejected.
- `/skills` exists (it was advertised); skills resolve from project and global dirs.
- Removed settings that were never wired to anything (theme, accordion, approval
  policy, git diff preview, smart commit) and tips that advertised missing features.
- Errors render as error lines instead of fake user messages; update failures
  are reported instead of swallowed.

### Updater and releases
- Downloads are verified against the release `SHA256SUMS`.
- A Windows `.zip` (or any package) is never written in place of the executable.
- The updater picks the newest release by version, never downgrades a beta in
  the background, never falls back to another platform's archive, and keeps
  updating when you work inside other Rust projects.
- Releases publish to rolling `beta` / `stable` channels with checksums and a raw
  Windows executable.
- CI was red on Windows (bash-only MCP tests); fixed.
