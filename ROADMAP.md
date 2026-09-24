# FlashAgent roadmap

v1 is the terminal app; the native UI (Track B) comes in v2. Local models come first, and cloud APIs must work properly for anyone whose machine cannot run a model.

**v1.0 ships when Track A is complete**, together with what Track C already ships: C0, the C3 packages, the C4 repository presentation and the C5 items marked done. The rest of Track C is listed under [v1.x](#v1x-after-the-first-stable-release). Track V was the groundwork that had to land first.

Status: `[ ]` not started, `[~]` in progress, `[x]` done. Milestones have no dates.

A milestone is closed only when its user path has been run end to end, in a scenario test or a live run, not when unit tests pass.

## Track V: ready for v1 (done in b266)

- [x] **V1. One command.** The binary is `flashagent` everywhere; the `flashagent-tui` alias and the empty `app` crate are gone.
- [x] **V2. MCP trust from config only.** Only `read_only` and `read_only_tools` in `.mcp.json` skip approval; tool names and a server's `readOnlyHint` do not.
- [x] **V3. `/goal` limits and report.** Step, time and token limits (now in Settings → Goal), their use shown in the status line, and an end-of-run card built from loop events: files touched, failed commands, why it stopped.
- [x] **V4. TUI structure.** `main.rs` went from 7,236 lines to about 1,500 plus 600 of tests, and its event loop from 3,349 lines to 86; every event has its own handler, and keys, submission, turn completion, rendering, sessions and menus are their own modules. Behaviour unchanged, checked with the full test suite and a live run.
- [x] **V5. Scenario tests.** The real binary in a pseudo-terminal (`portable-pty` + `vt100`) against a scripted model server, in `crates/tui/tests/scenarios.rs`, on Linux, Windows and macOS in CI. Each scenario was checked by breaking what it guards.
- [x] **V6. Tool-calling test.** `flashagent --tool-test [--all-models]` runs eight scenarios; results are in [docs/tool-calling.md](docs/tool-calling.md) and the README, and the first run checks the chosen model.
- [x] **V7. Fewer guesses.** What the server reports (reasoning presets, context, tool support) is used instead of guessing from names. Reasoning is not guessed from model names; a model the server says nothing about is "unreported" and left to its default. The kind of server comes from the API that answered, not from the port. Still guessed, on purpose: the local token counter picks its tokenizer by model name, since no server reports one before a request.

## Track A: core

- [x] **A0. Workspace.** Five crates (core, llm, tools, tui, svc), CI on Linux, macOS and Windows, clippy with no warnings, MIT license, README.
- [x] **A1. Data layer.** SQLite with FTS5. Never used by the TUI, whose sessions are JSON files; removed, last present in commit 28ee7ea.
- [x] **A2. Model client.** OpenAI-compatible streaming, one `ToolCall` type, a parser for calls written as text (Hermes, Mistral, bare JSON) with JSON repair, reasoning, token usage with a local estimate when the server gives none.
- [x] **A3. Agent loop.** `AgentLoop` over the `LlmSource` and `ToolExec` traits, UI events, cancellation, and contract tests for parallel calls, cut-off streams, limits and prompt injection.
- [x] **A4. Tools.** Files, search, `run_shell` (background tasks, timeouts that kill the whole process tree, live output), `web_fetch`, `web_search`. Commands run with the user's rights; there is no sandbox.
- [x] **A5. Permissions.** Four modes, shell commands parsed into their parts so a rule never covers a different command, a diff before every write, rules for the session, approval cards.
- [x] **A6. Memory.** Project and global memory, `MEMORY.md`, `CLAUDE.md` and `AGENTS.md` picked up, whole files or outlines depending on size, file contents marked as untrusted.
- [x] **A7. Terminal client.** Streaming chat, approval cards with diffs, Esc to interrupt, memory in the prompt; then settled lines drawn once, Ctrl+O for thinking, a status line.
- [x] **A8. Subagents.** Roles per task, a limit on how many run at once, the parent reviews their work, messages between them, permissions inherited and never widened.
- [x] **A9. MCP.** stdio JSON-RPC 2.0, project and global config, a small reviewed marketplace, approval for any tool not marked read-only, `/mcp` commands.
- [x] **A10. `/goal`.** Accept All with maximum effort, `ask_user` answered by the model after two minutes, memory writes off, mode and effort restored afterwards, limits with a live count, the report card (V3). File copies before every write and `/rewind`, with a card that shows each file's diff before anything moves (b267). Dangerous commands (`rm -rf`, `sudo`, force-push, `reset --hard`, `git clean -f`, `branch -D`, `dd`, `mkfs`, `shutdown`, `reboot`) ask even in Accept All and are refused during `/goal`. A git commit every 10 steps inside an existing repository. A live plan through `update_plan`, shown as a checklist.
- [~] **A11. Any provider, local or cloud.** One client for four protocols: OpenAI-compatible, and native Anthropic, Gemini and Ollama. Presets for LM Studio, Ollama, llama.cpp, vLLM, Jan, KoboldCpp, text-generation-webui, LocalAI, Anthropic, OpenAI, Gemini, OpenRouter, DeepSeek, Mistral, Groq, xAI, Together, Fireworks and Cerebras; any other `/chat/completions` server works as Custom. The client adapts to the server: a field it names in a 400 or 422 is left out from then on, 429/502/503/504/529 are retried after `Retry-After`. Saved providers, each with its protocol, address, key and model, the provider's usual environment variable as the fallback key, and `/provider` to switch mid-conversation without a restart; an older single-server config is read as one provider. **Open for v1.0:** each preset checked end to end with a real key: streaming, tool calls, reasoning settings, context from the model list.
- [x] **A12. Migrator.** Closed 2026-09-14 without work: there is no `~/.flashgent` data to import.

## Track B: native UI (v2)

Decided 2026-09-11: v1 ships the terminal app only, and B1–B10 resume after v1.0. The B0 prototype was removed from the repository and is in commit 28ee7ea.

- [x] **B0. Renderer prototype (gate).** Approved by the owner: wgpu, cosmic-text, IME and spring animation work.
- [ ] **B1. Material 3 Expressive kit.** Light, dark and system themes with a dynamic palette, Material Symbols, type scale, springs, ambient motion, a switch to turn motion off.
- [ ] **B2. App shell.** IPC client with reconnect, session sidebar with search, status line, command palette.
- [ ] **B3. Conversation.** Markdown while it streams, tool call previews, grouping, verbose and transcript modes, scroll anchoring, a growing composer with `@` and `/` completion.
- [ ] **B4. Approval cards.** Diffs unified, side by side, per line and live; a problems pane.
- [ ] **B5. Subagents.** Live tree, tabs, overlay.
- [ ] **B6. Memory.** Editor for both levels, diffs, which rule files are active.
- [ ] **B7. MCP.** Server manager, marketplace, cards that show a call's arguments.
- [ ] **B8. `/goal`.** Limits, live plan, final report.
- [ ] **B9. Setup wizard.** Server, connection check, model check, test tool call.
- [ ] **B10. Polish.** Density, a settings panel, virtualised lists for 100k+ messages, an AccessKit audit.

## Track C: product and distribution

- [x] **C0. Self-updater.** GitHub Releases, stable and beta channels, background download, atomic replacement of the binary, `/channel`, `/update`, a notice that does not interrupt the session. Downloads are checked against `SHA256SUMS`; background updates never move to an older beta; a package is never installed as a binary.
- [ ] **C1. Telemetry.** Moved to v1.x. None exists: the app sends no telemetry.
- [ ] **C2. Documentation site.** Moved to v1.x; the README and `docs/` are the documentation for v1.0.
- [~] **C3. Packaging.** Shipping: deb, pacman, AppImage and Void packages, macOS tarballs for Apple Silicon and Intel, a Windows zip, the install scripts and the icon, with `SHA256SUMS` on every release. Moved to v1.x: MSIX or NSIS installers, a dmg, code signing, macOS notarization.
- [~] **C4. Public launch.** Done: GIFs of real sessions, a quick start in the README, Discussions. Open: a feature comparison table and labelled good-first-issues.
- [~] **C5. From the old app.** Ideas carried over from the Electron flashgent. The ones not done move to v1.x.
  - Tolerant argument names (`filePath`/`path`, `oldString`/`TargetContent`, `cmd`/`command`). *Done (b268), in `effective_args`, so the approval card, the rules and the tool see the same call; MCP tools keep their own names.*
  - A project outline (types and functions) in the system prompt.
  - Steering: a message typed during a turn goes into it. *Done: Enter while a turn runs.*
  - Rewind. *Done (b267): `/rewind`, with file copies in `~/.flashagent/snapshots/<session>/`; files and conversation go back to before a chosen turn; shell changes and files over 8 MB are reported, not undone.*
  - Continuing a cut-off answer. *Done (b269): a reply cut off at the output limit is continued up to three times in the same message, with a repeated tail trimmed; a cut-off tool call is not run.*
  - Stronger prompt-injection defences: a per-session nonce, neutralised template control tokens, detection of hijack attempts.
  - Editing several files at once. *Done (b270): `edit_file` takes `files: [{path, edits}]` and applies all or none; `read_file` reads up to 20 files; the diff card, `/rewind` and the `/goal` report cover every file.*
  - A title for each session, asked of the model after the first turn.
  - Status line extras. *Done: generation speed over the last 3 seconds, and the Ctrl+K command palette (b340).*

## v1.x: after the first stable release

Planned, not promised for v1.0:

- **Telemetry (C1):** opt-in counters and crash stack traces. Nothing is sent until it exists and you turn it on.
- **Documentation site (C2):** mdBook on GitHub Pages, rustdoc, MCP guides.
- **Installers and signing (C3):** MSIX or NSIS, a dmg, code signing, notarization.
- **The rest of C5:** the project outline, stronger prompt-injection defences, session titles.

## How milestones are worked

- One milestone, one branch, one result that can be checked. Within a track, milestones go in order.
- Track B did not start without the B0 prototype.
- Every milestone passes `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`.
