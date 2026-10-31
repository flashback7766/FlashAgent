# FlashAgent — handoff for the next agent

You are continuing development of FlashAgent, a local-first TUI coding agent
in Rust. This file is the entry point another agent left for you; the repo's
`memory/` directory holds the older eras, and this one holds the present.

The owner (flashback7766) runs the beta and talks to you in Russian. He is
hands-on: he files bugs from live sessions with screenshots, asks for
features mid-flight, and expects you to keep up.

## The owner's standing rules — break none of them

- **Beta only.** «Работаем в бета бренче, стабильную не трогаем» — all work
  lands on `main` as rolling beta builds; the stable channel is cut with
  `bump.sh stable` and is the owner's call, never yours.
- **Fix + test.** No change ships without a test that fails before it. CI
  runs the workspace on Linux, macOS and Windows; clippy must stay clean.
- **No `cargo fmt`.** The codebase is hand-formatted; keep the surrounding
  style.
- **Why-comments, in English.** A comment says why the code is what it is,
  never what the next line does or that you changed something. User-facing
  text (UI strings, notices, hints) is **Russian** via `crates/tui/src/i18n.rs`
  — English is the key, the table holds the translation; `untranslated.rs`
  enforces that every shown phrase is translated or allow-listed.
- **CHANGELOG first.** Every release needs a `## bN — title` section in
  `CHANGELOG.md` before `packaging/bump.sh` will tag it.
- **Discuss design before implementing.** The owner once stopped a build
  with «стоп, а со мной обсудить?». For anything with real product choices,
  lay out the options and ask (recommended option first) before writing code.
- Never delete the owner's data without asking; never touch his
  `~/.flashagent/config.json` API keys.

## Repo layout

- `crates/core` — config, agent loop, prompt builder, compaction rules,
  permissions, plan, memory store. Depends on `llm`.
- `crates/llm` — one model client for every protocol (OpenAI-compatible,
  Anthropic, Gemini, Ollama): streaming, adaptive retries, server discovery
  (`thinking.rs` parses model lists from many server shapes).
- `crates/tools` — the tools the model may call (shell, fs, web, MCP,
  subagents).
- `crates/tui` — the terminal UI. Note the split: `lib.rs` (ChatView,
  welcome card, backend_error, i18n) is the *library*; `main.rs`,
  `events.rs`, `keys.rs`, `render.rs`, `overlay*.rs`, `select.rs`,
  `settings.rs` form the *binary* — types cross that boundary via
  `flashagent_tui::…`, not `crate::…`.
- `crates/svc` — the updater and versioning.
- `packaging/bump.sh` — the only release path. `memory/` — one file per era
  of this repo's own history; read `flashagent-b606-audit-and-v1-readiness.md`
  for pre-b618 findings.

## Releasing a build

1. Write the `## bN — title` CHANGELOG section (it becomes the release
   notes). Sizes: `mini` +1, `medium`/`5` for a batch, explicit number for
   more.
2. Commit everything else first — bump.sh refuses to run over uncommitted
   changes besides CHANGELOG.
3. `packaging/bump.sh mini` commits, tags `bN`, prints the publish command.
4. `git push origin HEAD 'bN'` — the Release workflow runs all tests on
   three OSes before publishing under the moving `beta` tag.
5. If a test fix lands after the tag: delete the tag locally and remotely,
   re-tag on the new tip, push again (the workflow re-runs). Done twice
   recently; it is the established procedure.

## Where things stand (after b627)

Recent builds, newest first:

- **b627** — a compaction's standing line ("…about 0 tokens freed") is
  earned, not owed: it appears only when the compaction freed ≥ 256 tokens;
  the summary card alone is the result of a small one.
- **b626** — hover and press looks for every clickable cell; an amber ✕ in
  the top-right corner of every overlay card closes it.
- **b625** — the mouse reached everything: select menus (palette, model,
  effort, provider, sessions) answer wheel + click; the welcome card's own
  buttons (mascot → Settings, model → model menu, Quick Commands keys act
  as keys, `/goal` `/help` type themselves); the context gauge opens F1's
  menu; the "more lines below" marker returns to the bottom; the scrollbar
  shows itself only near the right edge. Two voices joined: Cybersecurity
  and Tsundere. The mascot is named **Swift**.
- **b620** — backend errors are parsed, not dumped: the provider's sentence
  is dug out of any JSON shape (`backend_error.rs`), wrappers stripped,
  status codes and known situations mapped to hints.
- **b619** — the "git stash" bug: a fake voice-prelude exchange used to ride
  every request; the voice sample is now quoted inside the system prompt.
  Never put fake earlier exchanges in a model's context.
- **b618** — OpenCode Go is the first cloud preset
  (`https://opencode.ai/inference/go/v1`, `OPENCODE_API_KEY`). Its Go routes
  **require** the `x-opencode-session` header and a real User-Agent; the llm
  client sends both (session id per conversation, survives resume and
  provider switches, only to `opencode.ai` hosts). Their model list is
  parsed whole (`limit.context`, `capabilities`, `modalities`).

The README status line says the current beta build; bump.sh keeps it true.

## What comes next

- **Phase 2 — themes** (the owner's stated next want): JSON palettes in
  `config.json` + a `/theme` command. Start from `crates/tui/src/theme.rs`
  (set/active/recolor at the top) and `ColorTheme` in `crates/core/src/config.rs`
  (~line 605). Discuss the palette format with the owner first.
- **Phase 3 — rendering polish.** Remaining candidates: making the rows of
  the providers/sampling/memory/mcp/tasks screens clickable (b625/626 gave
  them the ✕ and, for menus, hover — their *rows* still answer keys only);
  click targets on question/approval cards; the frame floats above empty
  rows when the transcript is shorter than the screen (bottom-anchoring —
  careful, the click coordinate system assumes screen row == rows-vector
  index; the overlay_row off-by-one history is in paint_layout's comments).
- Text layout hardening for long/RTL languages needs the owner's
  screenshots first.

## Lessons that cost someone time — read before debugging

- **PTY probes:** `cargo build --release` *before* probing — `cargo check`
  does not rebuild the binary, and you will test a stale one. Probe homes go
  in `/var/tmp` (tmpfiles cleans `/tmp` within minutes). Bypass the wizard
  with a pre-written config (`"setup_completed": true`, `backend_url`,
  `model`, `last_seen_version`) and `FLASHAGENT_TRUST_DIR=1`. A pty.fork has
  **no winsize** — set TIOCSWINSZ or nothing renders; answer terminal
  queries (kitty keyboard `\x1b[?u`, DA1) or the app may block.
- **Scenario tests** (`crates/tui/tests/scenarios.rs`, real binary in a
  pty): `Term::find_on_screen` + `Term::click(row, col)` (SGR mouse) exist —
  use them instead of guessing coordinates. SGR rows/cols are 1-based;
  crossterm's are 0-based. `find_on_screen` returns cell numbers, not byte
  offsets. Wait for a string to *exist* before locating it (the welcome
  card's reveal animation draws a row at a time). Ctrl+D quits only an empty
  prompt — clear typed text first. Windows CI caught: platform-dependent
  expectations go through `cfg!()`; the Release CI compiles tests with a
  longer `FLASHAGENT_VERSION`, so layouts must fit whatever it says.
- **Renderer coordinates:** the painted rows vector is indexed by screen
  row; `cut` (rows drained from the top) and `overlay_row` arithmetic are
  where the historic off-by-ones lived. `Renderer` stores click targets
  measured at paint time (`status_gauge`, `scroll_marker`, `overlay_close`,
  `chat_view`) — follow that pattern for anything new that clicks.
- **Visibility across the lib/bin split:** types used by both must be `pub`
  in `crates/tui/src/lib.rs` (re-exported via `pub use welcome::*` etc.).
  A `pub(crate)` type in a `pub` struct field is a compile error on the bin.
- **Prompt hygiene:** nothing fake rides the context (b619). The voice
  sample is quoted in the system prompt with an explicit "its subject never
  comes up" line; keep it that way.
- **Compaction:** the standing line is earned (b627); a single exchange can
  still be compacted by hand (`a_single_exchange_can_be_compacted_too`
  scenario pins it — do not "fix" it into a refusal).
- **Live testing:** `OPENROUTER_API_KEY` is in the environment; the owner's
  OpenCode Go key lives in his config — use it only through an isolated
  `HOME=/var/tmp/… FLASHAGENT_CONFIG_PATH=… ./target/release/flashagent
  --tool-test`, one request, never against his live session. Free models
  from outside OpenCode's own client get 403.
- The nightly "Web tools" workflow goes red on DuckDuckGo bot challenges;
  that is the internet, not the code. CI red on all three OSes at once is
  usually a real test failure — read the assertion, not the title.

## The owner's environment

CachyOS, zsh, JetBrains Mono, terminal ≥ 100×35. He runs the beta through
the in-app updater. His OpenCode Go preset is active (DeepSeek V4.1 Flash
lately); a local LM Studio config is used for offline scenarios. When he
pastes a screenshot with Cyrillic gibberish, it may be Russian typed on an
English layout — decode via ЙЦУКЕН before asking.

Update this file when the "where things stand" section stops being true.
