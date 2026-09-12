# Changelog

Beta builds are numbered `bNNN` and ship on the rolling `beta` pre-release.
Stable versions start at `v1.0.0` and ship on the rolling `stable` release.

## Unreleased

- Changing the release channel now asks first. It replaces the binary with a
  different line of builds, so the card says what that means — moving down to
  stable can take features away and reset the settings they introduced — and
  names the version you would land on. Answering no puts back only the
  channel: everything else you changed in the same visit stays applied. If the
  channel you picked has nothing published on it yet, it says that instead of
  pretending there is a version waiting.

- Messages that appear on their own — an update installing, the context being
  compacted — now live on the line under the input rather than in the composer.
  The composer answers what you just did; something you did not ask for must
  not take that spot. They also share the line with the live token counters
  while a turn is running, instead of waiting for it to finish, and the ones
  that stop being actionable dim out over their last second rather than
  blinking away.
- The recap and the follow-up suggestion now come back in the language of the
  conversation. "Write it in the language of the conversation" is not an
  instruction a small model follows; the language is now named outright,
  detected by script — and a Russian conversation that quotes Python still
  reads as Russian.

- `flashagent --tool-test` scores your model on eight scenarios the agent loop
  performs on real work, and `--all-models` does it for every chat model your
  server lists and prints a markdown table. The check also runs once after
  setup, so a model that cannot drive tools is something you learn in the first
  minute rather than the first hour. Results for four local models, the method
  and the raw output are in [docs/tool-calling.md](docs/tool-calling.md).

- One-line system notices moved out of the transcript and under the cursor.
  `[No models discovered]`, `[Usage: /verbose …]`, `Update b238 · [███░░] 52%`
  and the rest are addressed to the person at the keyboard, not to the
  conversation, and a chat full of them is a chat you stop reading. Anything
  with structure — a skills listing, a git diff, the `/goal` report — still
  belongs in the transcript and stays there.
- A finished update no longer says the same thing twice: the progress line
  disappears and the banner states the outcome.

- A single bad line in `config.json` no longer resets every setting. The
  loader used to throw the whole file away on any error, so one typo silently
  put you back on the default backend, model and permission mode. Now each
  field stands on its own: the broken ones fall back and are named on stderr,
  everything else is kept.
- Stored values are read whatever their case. `"Beta"`, `"beta"`,
  `"Accept Edits"` (the label the app itself shows) and `"accept_edits"` all
  mean what they look like — a config is edited by hand, and a capital letter
  was costing people their settings.
- CI keeps its build artifacts for three days instead of the default ninety.
  They only exist to hand binaries from the build jobs to the publish job in
  the same run — the release assets are the lasting copy — and two days of
  releases had filled the account's Actions storage with 2.3 GB of them.
- The README recordings are redone against the current build, and two were
  added where a still picture explains nothing: `/goal` being stopped by its
  step budget and handing back a report, and Ctrl+U downloading and installing
  an update. Each one is a real session against a local model, so the timings
  and token counts on screen are the ones it produced.

## b238 — no emoji in the tool cards

- Tool cards drop their emoji icons. `Edited notes.md +1 -0`, `Read
  src/main.rs (120 lines)`, `Analyzed src/` — the extension already says what
  a file is, so the icon carried nothing, and being two columns wide next to
  one-column glyphs it made listings fail to line up. Directories still end in
  `/`, the way `ls -F` marks them. A test keeps them out.

## b237 — no lightning left

- The last two lightning bolts are gone: the welcome card says `FlashAgent
  Engine` plainly, and a shell script is marked `$` rather than an emoji —
  which is also one column wide instead of two.

## b236 — updates you can watch

- The status line drops the lightning emoji from its numbers: `TTFT 6.84s
  (495 t/s prefill)`, `Prefill ~1.2s`, `cache 62%`. A telemetry row is meant
  to be read, not decorated.
- Ctrl+U (and `/update`) now checks, downloads and installs in one go, with
  the download visible: `Update b235 · [████████░░░░░░░░] 52% · 3.4/6.6 MB`,
  then `verifying checksum...`, `installing...`, and the line lands on
  `installed · restart FlashAgent to run it`. It rewrites one line rather than
  stacking up, and a background update stays silent as before.
- Fixed: an installed `flashagent` refused to update while you stood in the
  FlashAgent repository, claiming to be a dev build. What decides that is
  where the binary lives, not where you are — and the repository is exactly
  where you are when you notice there is a new build.
- Fixed: opening FlashAgent and closing it without saying anything saved a
  session and offered a `--resume` id that restored nothing. The history is
  never really empty — it opens with the system prompt — so the check now
  looks for an actual user message.

## b235 — budgets, a mascot that means something

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
