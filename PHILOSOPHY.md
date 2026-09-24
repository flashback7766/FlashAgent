# FlashAgent philosophy

This file says what FlashAgent is and why. [ARCHITECTURE.md](ARCHITECTURE.md) says how it is built, [ROADMAP.md](ROADMAP.md) in what order. Where they disagree, this file wins, and it wins over any agent's memory too.

**v1 is the terminal app; the native UI is v2** (owner decision, 2026-09-11, amended 2026-09-22). Where this file describes the native renderer, its processes and its storage, it describes v2. The terminal app follows the same principles wherever a terminal can: full editing in the prompt, one switch for all motion, the status line, a key for everything.

## 1. What it is

A local coding agent that speeds a person up; the person leads. `/goal` turns it into an autonomous agent that works inside limits the task sets.

- By default: a chat with tools.
- `/goal`: runs the task end to end. A simple task is a plain loop; a large one gets a plan, milestones, checking its own work and a report at the end.
- Four permission modes: Planning (reads only), Manual (asks before anything that is not a read), Accept Edits (file changes run, the shell asks), Accept All.

## 2. What it will not have

1. **Features that do not serve "person → task → result".**
2. **Unreliable tool calling.** A call must never depend on luck.
3. **Electron,** web views or a bundled Chromium.
4. **Rough interface details.** Every interaction is held to Material 3 Expressive.

## 3. Network

Models and data stay on the machine by default. What FlashAgent sends out:

- Update checks and downloads from GitHub Releases, verified against checksums and installed in the background for the next launch. Can be turned off in Settings.
- Requests to the model provider the user chose, local or cloud.
- MCP servers the user configured. A tool that is not marked read-only asks first and shows its arguments.
- `web_fetch` and `web_search`: on by default and treated like reading a file in every mode; can be turned off in Settings.
- Telemetry: none. If it is ever added, it is opt-in and limited to a launch count, version, OS and which backends and tools are used, never session content; crash reports opt-in, stack traces only.

## 4. Technology

| Layer | Decision |
|---|---|
| Core | Rust (tokio, serde) |
| UI | v1: terminal UI (crossterm), the product itself. v2: own wgpu renderer; the design comes before any framework's defaults |
| Text (v2) | cosmic-text or parley: shaping, CJK, RTL, emoji, ligatures |
| Theme | Material 3 Expressive. v1 in terminal colours, with themes for 16-colour and monochrome terminals; v2 in full, including buttons that expand into small panels |
| Fonts (v2) | Bundled Roboto Flex or Inter, JetBrains Mono, Noto Emoji; the user's own TTF/OTF |
| Icons (v2) | Material Symbols |
| Data | v1: plain files under `~/.flashagent/` (sessions as JSON written atomically, snapshots, config). v2: SQLite with FTS5 for session search |
| Processes | v1: one process. v2: a UI process and a core service over IPC, so a UI crash never stops a running task |
| Models | HTTP: OpenAI-compatible servers plus the native Anthropic, Gemini and Ollama APIs. Embedded llama.cpp, with grammar-constrained tool calls, is a later milestone, after the app works without faults |
| TUI in v2 | Stays, as a client of the same core, for terminal users and for automated tests |
| License | MIT |
| Documentation | README as the entry point; a generated site (mdBook on GitHub Pages) later |
| Releases | Beta builds numbered `b<N>`, stable releases SemVer cut from a build (`vX.Y.Z+b<N>`); see [VERSIONING.md](VERSIONING.md) |
| Language | The interface and the repository are English only; the model answers in the language the user wrote in |

## 5. Tool calling

- One internal `ToolCall` type, with adapters for native JSON calls, XML tags and NDJSON blocks; the same JSON format is parsed whichever way it came.
- The parser recovers what it can today; grammar sampling comes with embedded llama.cpp.
- Contract tests against a mock server cover cut-off streams, broken JSON, prompt injection and unclosed blocks. No test depends on a live model.

## 6. Permissions

- Shift+Tab cycles the four modes. `/goal` runs in Accept All within its limits and restores the previous mode afterwards.
- Approval cards offer Allow, Always and Deny. Always stores a narrow rule for the session (`npm test` is not `npm publish`). A file change always shows its diff first.
- A subagent has its parent's permissions and can never gain more.

## 7. `/goal`

- Limits: steps, generated tokens and time, all set in Settings and unlimited by default; dangerous commands are refused outright. A question to the user waits two minutes, then the agent picks the most reasonable answer itself.
- A live plan on screen and a report at the end: done, left out, needs checking by hand.
- Files are snapshotted before the run and committed at milestones.
- Subagents: roles chosen per task, a limit on how many run at once, the parent reviews their work, they can message each other; the UI shows them as a live tree with tabs.

## 8. Memory

- Project memory (`MEMORY.md` in the repository) and global memory (`~/.flashagent/MEMORY.md`).
- Rule files other tools use (`CLAUDE.md`, `AGENTS.md`) are picked up too.
- Changing memory is a file change and follows the permission mode like any other.
- Whole files go into the context when they fit, outlines when they do not; the model reads the rest with tools.
- UI: a memory editor showing both levels, diffs of changes, and which rule files were found.

## 9. Tools

Chosen by the model's context window: a minimal set around 32k tokens, a core set around 64k, the full set above. MCP servers add their own.

## 10. Interface

This section describes the v2 native UI. The terminal app keeps every point a terminal can show, and ROADMAP says where it cannot.

- Material 3 Expressive, followed literally, on the own wgpu renderer.
- Motion: spring physics, quiet ambient loops, state morphs (Stop ↔ Send, a diff opening), and one switch that turns all of it off.
- Compact and comfortable density.
- Tool calls: a live preview, consecutive calls grouped, a verbose mode (thoughts in grey, code blocks full width) and a transcript mode.
- Scrolling that follows new output, keeps its place and moves smoothly.
- A prompt that grows up to a limit and supports full editing.
- One status line: current action, tokens, speed, context used.
- Every action on a key; mouse and touch equally usable.
- Errors as an inline card with Retry, and a diagnostics pane.
- Subagents as a live tree, with tabs and a counter.
- Sessions in a sidebar with instant search.
- MCP: a settings manager, a marketplace and approval cards that show the arguments.
- Diffs unified or side by side, selectable by line, updated live.
- Markdown rendered while it streams.
- Reasoning models: thinking shown in grey while it streams, in full in the transcript.
- Token counts from the server, estimated with tiktoken when it reports none.
- First launch: a setup wizard that picks the server, checks the connection and the model, and makes a test tool call.

## 11. Performance

Measured figures for the terminal app are in [docs/numbers.md](docs/numbers.md). The display targets (refresh rate, DPI, AccessKit) are for v2.

- Cold start under a second.
- Frames at the monitor's refresh rate (VRR, LTPO, displays plugged in while running); no frame drawn when nothing changed.
- Sharp text at fractional scaling (125%, 150%); moving between displays live.
- Sessions of 100k+ messages scroll without dropped frames.
- Memory well under Electron's 100–200 MB without a model; a soft target.
- IME (Cyrillic, CJK) and AccessKit from the first release.

## 12. History and v1

- The old Electron app, flashgent, is frozen. It is kept as a reference for components proven in use, such as parsers and safety checks.
- This repository is `flashagent`; the product is FlashAgent.
- The core was built bottom-up, tested through the TUI, alongside a UI renderer prototype. The prototype passed its gate (B0); on 2026-09-11 the owner decided v1 ships the terminal app and the native UI resumes after v1.0.
- Moving data from `~/.flashgent` was dropped (ROADMAP A12): there was nothing to move.
- v1 is done when the author uses it for daily work, the public release is good enough to draw a community, and others build MCP servers and themes for it.
- Milestones have no fixed dates.

## 13. Order of work

This file, then ARCHITECTURE.md and ROADMAP.md detailed enough to hand to an agent, then code, with the owner checking each step. The decisions above come from about 110 answers the owner gave across 9 areas.
