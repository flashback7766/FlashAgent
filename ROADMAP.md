# FlashAgent — Roadmap (Dateless Milestones)

> Sequence: two parallel tracks. Core engine is verified via the TUI client; UI is verified via the renderer prototype.
> Milestone status: `[ ]` / `[~]` / `[x]`. Updated upon completion, without arbitrary calendar deadlines.

## Track A — Core Engine
- [x] **A0. Workspace Skeleton** — 9 crates, CI (Linux + Windows), clippy strict with 0 warnings, MIT license, README.
- [x] **A1. Data Layer** — SQLite + FTS5 (bundled), sessions/messages, versioned migrations, FTS triggers, Cyrillic/multilingual search. 5/5 tests.
- [x] **A2. LLM Adapter** — OpenAI-compatible streaming, unified `ToolCall`, multi-parser (Hermes / Mistral / bare JSON) with self-healing JSON repair, reasoning, token usage with local fallback. 20/20 llm tests.
- [x] **A3. Agent Loop** — `AgentLoop` built over abstract `LlmSource` / `ToolExec` traits, UI event streams, cancellation token, 9/9 contract tests (multi-tool calls, stream truncation, step/token budget limits, prompt injection safety).
- [x] **A4. Tools** — 9 built-in tools (read / write / edit-chunks / list_dir / glob / grep / run_shell / web_fetch / web_search), process sandboxing (background execution, timeouts, live stream output, task id tracking). 13/13 tools tests, 57 workspace tests. Adaptive sets in Track C.
- [x] **A5. Permissions** — 4 security modes, narrow shell command parsing (parse_chain, quote handling, prefix bounds), diff preview pipeline (WritePreview + unified diff before writing), session-scoped rules, ApprovalGate for UI. 23 core tests (10 permissions + 4 diff), 68 workspace tests.
- [x] **A6. Memory** — Dual-tier memory, auto-discovery of MEMORY.md / CLAUDE.md / AGENTS.md, threshold-based context injection (full text → outline + targeted tool lookup), untrusted content block markers. 5 memory tests, 73 workspace tests.
- [x] **A7. TUI Client** — Terminal client (crossterm): streaming chat, interactive confirmation cards with diffs (TuiGate), Esc cancellation, memory injection. Smoke-tested on Linux. 4 tui tests, 77 workspace tests.
- [x] **A7.1. TUI Polish** — Print-and-forget renderer (settled lines flushed to scrollback once), ctrl+o thinking toggle, spinner/status bar, Esc interrupt, interleaved reasoning/content delta fix (parallel streaming), settled_boundary = min(). 8 tui tests, 81 workspace tests.
- [x] **A8. Subagents** — Dynamic roles, concurrency limits, parent review gate, inter-subagent messaging channels, strict permission inheritance.
- [x] **A9. MCP** — Client, manager, marketplace registry. Native stdio JSON-RPC 2.0 transport, project & global configuration manager, curated verifiable marketplace, ApprovalGate enforcement for non-read-only calls, TUI slash commands (/mcp, /mcp list, /mcp market, /mcp test, /mcp add, /mcp reload). 46 tools tests, 131 workspace tests.
- [ ] **A10. Autonomous Goal Mode** — `/goal`, adaptive execution layers, task boundaries (steps/tokens/time/blacklist), filesystem snapshots + commits, live plan + final report.
- [ ] **A11. Extended Backends** — Ollama, Anthropic, Mistral, DeepSeek, OpenRouter, configuration presets.
- [ ] **A12. Migrator** — Import data and settings from legacy ~/.flashgent (sessions, config).

## Track B — Native UI (Post-Prototype)
- [x] **B0. Renderer Prototype (Gate)** — Owner GO approved: wgpu + cosmic-text + IME + spring morphing verified, sRGB/color fix and spacebar handling resolved, clippy strict 0 warnings.
- [ ] **B1. Material 3 Expressive Kit** — Themes (light/dark/system, dynamic palette), Material Symbols, typography scale, spring engine, ambient background loops, motion reduction toggle.
- [ ] **B2. App Shell** — IPC client, auto-reconnect, session sidebar with FTS search, status ticker ribbon, command palette.
- [ ] **B3. Conversation Stream** — Live markdown renderer, peek tool calls, batch grouping, Verbose / Transcript modes, scroll anchoring, auto-growing composer with @// autocomplete.
- [ ] **B4. Permission Cards** — In-stream confirmation cards, diff renderer (unified / split / per-line / live), problem center.
- [ ] **B5. Subagents UI** — Live card tree + dedicated subagent tabs + overlay HUD.
- [ ] **B6. Memory UI** — Memory editor, dual tiers, diff inspector, active rules indicator.
- [ ] **B7. MCP UI** — Server manager + marketplace browser + execution preview cards.
- [ ] **B8. Goal UI** — `/goal` task envelope settings, live interactive plan document, final structured summary.
- [ ] **B9. Onboarding Wizard** — Backend selection → connection check → model verification → test tool execution.
- [ ] **B10. Polish** — Adaptive density, settings panel (GUI-first), virtualization for 100k+ message sessions, AccessKit accessibility audit.

## Track C — Product & Distribution
- [x] **C0. Self-Updater** — GitHub Releases, Restart to Update, Stable / Beta channels. Background download, atomic binary replacement (dual-target system/user), channel switching (/channel), manual update (/update), in-app banner without interrupting session.
- [ ] **C1. Telemetry** — Opt-in anonymous counters, opt-in crash stack traces.
- [ ] **C2. Documentation** — mdBook documentation site on GitHub Pages, rustdoc, MCP guides and cookbooks.
- [ ] **C3. Release Packaging** — Native installers (MSIX / NSIS for Windows, AppImage / deb / pacman for Linux, dmg for macOS), application icons, code signatures.
- [ ] **C4. Public Launch** — Repository presentation: demo screenshots and GIFs, feature comparison matrix, quickstart < 5 minutes, good-first-issues, GitHub discussions.
- [ ] **C5. Legacy Gems** — Porting battle-tested capabilities from legacy ~/flashgent:
  - **Argument Aliasing**: Tolerant tool parameter naming (`filePath`/`path`, `oldString`/`TargetContent`, `cmd`/`command`) in `flashagent-tools`.
  - **AST Project Outline**: Background symbol indexing (classes, functions, structs, traits) in native Rust (`ignore` + regex) with compact project outline injection into system prompt.
  - **Context Steering**: Mid-flight user directive injection channel into `AgentLoop` during streaming/execution without resetting context.
  - **FileSnapshots & Workspace Rewind**: Persistent `file_snapshots` table in SQLite (`content_before`) for step-by-step filesystem rewinds and history truncation.
  - **In-place Continuation (`continueResponse`)**: Seamless completion of truncated responses within the same assistant message card without message duplication.
  - **Deep Prompt Injection Hardening (`untrusted`)**: Per-session cryptographic nonce + template control token neutralisation + signature-based prompt hijacking detection.
  - **Batch Multi-File Editing**: Extending `edit_file` with batch file array support (`files: [{ path, edits }]`).
  - **Automatic Session Titling**: Asynchronous background micro-query to LLM after turn 1 to title the conversation.
  - **UI Telemetry Markers**: Rolling generation speedometer `tg_3s` (3-second window) and command palette `Cmd/Ctrl+K`.

## Execution Rules
- One milestone = one branch = verifiable outcome. Sequence within tracks follows milestone numbering.
- Gate B0 is mandatory: Track B does not proceed without a validated prototype.
- Verification for every milestone: `cargo test && cargo clippy -- -D warnings` (UI: golden frames).
