# FlashAgent — Architecture

> HOW we build. Source of WHAT and WHY is `PHILOSOPHY.md`. Sequence of execution is `ROADMAP.md`.

## 0. Implementation Status (b237)

This document describes the target design. What exists today:

- **Process model**: not split yet. `flashagent` (crate `flashagent-tui`) hosts loop, tools and permissions in one process; `proto` (IPC contract) is a placeholder, `svc` contains only the updater. The GUI entrypoint crate was removed until Track B resumes (v2).
- **Data**: `crates/data` (SQLite + FTS5) is implemented and tested but not used by the TUI; sessions are JSON files in `~/.flashagent/sessions/`.
- **LLM**: OpenAI-compatible backend only (A11 adds native adapters). The text tool-call parser runs inside `core::loop_`.
- **Memory writes**: the `memory_*` tools go through the Write category; they are disabled during `/goal`.
- **Renderer**: B0 prototype only (`crates/ui/src/bin/b0.rs`); no component kit, no golden frames yet.

## 1. Process Model

```
┌─────────────┐   Local IPC         ┌──────────────────┐
│  UI Process  │ ◄────────────────► │  Core Service    │
│  flashagent- │   events/commands  │  flashagent-svc  │
│  ui (wgpu)   │                    │  loop, tools, DB │──► LLM Backends (HTTP)
└─────────────┘                    └──────────────────┘──► MCP Servers
       ▲                                   ▲
       │ same protocol                     │ shell processes (sandboxed)
┌──────┴──────┐                            ▼
│ TUI Client  │                    SQLite + FTS5
└─────────────┘
```

- A UI crash never kills the task: the background service continues the agent loop, and the UI reconnects, restoring full state from SQLite plus event replay.
- IPC Transport: Named pipes (Windows) / Unix domain sockets (Linux/macOS), length-prefix framing, serialization via serde JSON initially (with room for postcard binary serialization later).
- TUI uses the exact same protocol and core service: the core engine is validated without a GUI from day one.

## 2. Workspace (Cargo Crates)

| Crate | Responsibility |
|---|---|
| `core` | Agent loop, modes, permissions, subagents, memory, autonomy layers. Zero dependencies on HTTP, SQL, or UI. |
| `llm` | `LlmBackend` trait, adapters (OpenAI-compatible, Ollama, Anthropic, Mistral, DeepSeek, OpenRouter…), unified `ToolCall`, parsers, reasoning stream, token usage. |
| `tools` | `Tool` trait, built-in toolsets (adaptive by context window size), MCP client, registry/marketplace. |
| `data` | SQLite + FTS5, schema migrations, sessions, memory, export, legacy data migrator from ~/.flashgent. |
| `proto` | IPC protocol contract: UI→Core commands, Core→UI events, strongly typed and versioned serde schemas. |
| `svc` | Core service binary: tokio runtime, shell isolation, filesystem snapshots, updater engine, telemetry. |
| `ui` | wgpu hardware renderer, cosmic-text/parley, Material 3 Expressive component kit, spring physics engine, AccessKit, IME. |
| `tui` | Lightweight terminal client connected to the core engine. |
| `app` (v2, not present) | Main entrypoint binary: manages service lifecycle and launches the native UI. |

Dependency Rule: `core` does not depend on `llm` or `ui` — the loop consumes abstract traits. `ui` knows nothing of HTTP or SQL.

## 3. LLM Layer

- Trait: `stream(request) -> Stream<Event>`; event stream variants: `TextDelta`, `ReasoningDelta`, `ToolCallChunk`, `Usage`, `Error`, `Done`.
- Unified `ToolCall { id, name, args_json }`; protocol adapters: native JSON tool calls, XML tags, NDJSON blocks. A shared multi-parser with self-healing JSON repair handles normalization prior to adapters.
- Reasoning: `reasoning_content` / `<think>` tags — first-class support, stored in history, rendered muted grey in stream and complete in Transcript.
- Tokens: reported usage from API with local fallback estimation (tiktoken lookup tables).
- Universal compatibility with any OpenAI-compatible endpoint. Backend presets are configuration, not hardcoded logic.

## 4. Tools and MCP

- Adaptive toolsets: `ultra (~32k)` / `core (~64k)` / `full` — dynamically chosen based on model context limits, overrideable by user.
- MCP: client powered by `rmcp`, settings manager, marketplace curated from a verifiable JSON registry in repo. Non-read-only calls require confirmation with argument preview.

## 5. Permissions and Safety

- Four primary modes: Manual / Autonomic / Planning / Bypass + optional category matrix (`read` / `write` / `shell` / `net` / `mcp`).
- Allow rules: narrow execution rules (command prefix matching with full parsing of `&&`, `;`, `|` chains; e.g. `npm test` never covers `npm publish`).
- File writes: mandatory diff preview before writing (unified / split view, per-line selection, live diff).
- Subagents: inherit parent permissions strictly; privilege escalation is architecturally prevented (permissions reside in execution context, not model prompts).
- Autonomous mode: filesystem snapshot per task + git commit per milestone; bounded by step limits, token budgets, execution timeouts, and action blacklists.
- Prompt injection protection: tool content is unconditionally treated as untrusted (`Untrusted<T>`); system directives embedded inside data payloads are ignored by design.

## 6. Memory

- Dual-tier architecture: project-level `MEMORY.md` + global `~/.flashagent/MEMORY.md`; automatic discovery of external rule files (`CLAUDE.md`, `AGENTS.md`).
- Context injection: full inclusion when within budget; otherwise table of contents with targeted tool-based lookup.
- Modification: requires explicit confirmation in Manual mode, automated in Autonomic mode; memory diffs clearly rendered in UI.

## 7. Renderer (Highest-Risk Layer)

- wgpu + cosmic-text / parley. Bespoke Material 3 Expressive widget system: morphing buttons, mini-panels, dynamic color schemes.
- Animations: global spring physics engine; timeline scheduler; non-distracting ambient background loops; parallel transitions; master toggle to reduce/disable motion.
- Frame scheduling: present on change (sleep frames), vsync / VRR, render rate matching monitor native Hz, dynamic display switching, crisp fractional DPI scaling.
- Text input: IME via winit from day one; AccessKit accessibility from day one; Material Symbols; Noto Emoji; bundled typography with custom TTF/OTF support.
- Validation gate: 1-week isolated prototype (window + cosmic-text + composer with Cyrillic/IME + single spring morph) prior to committing to main codebase.

## 8. Updates and Telemetry

- GitHub Releases: check → download → verify checksum/signature → "Restart to Update" banner → atomic replacement on next launch. Stable and Beta channels.
- Telemetry: opt-in operational counters (launch count, version, OS, backend/tools), opt-in crash stack traces. Zero session logs or user data sent.

## 9. Testing and Quality Assurance

- Loop contract tests: deterministic mock LLM server exercising edge cases (stream drops, corrupted JSON, prompt injections, unclosed blocks, multi-tool executions).
- Golden traces: recorded conversation sessions serving as regression benchmarks for parsers.
- UI: golden render frames and layout constraint tests. `cargo test` + CI on GitHub Actions across Linux and Windows.
