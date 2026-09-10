# FlashAgent — Context for AI Agents (Compaction-Safe)

> Purpose of this file: any new agent (or this same agent following context window compaction)
> reads this document to immediately resume work without losing context or intent. Update upon closing every milestone.

## What is This Project
A complete rewrite of flashgent (Electron, legacy in `/home/flashback/flashgent-dev`) into
**`~/FlashAgent`**: a fast, local-first agent amplifier with `/goal` autonomy, a native Rust core +
custom hardware-accelerated wgpu renderer (Material 3 Expressive), zero Electron, zero web views. Open source (MIT).

## Hierarchy of Truth (Read in this strict order)
1. `PHILOSOPHY.md` — The Canon: WHAT and WHY (~110 explicit owner decisions; modify only with explicit instruction from the owner)
2. `ARCHITECTURE.md` — HOW: 9 crates, strict crate boundaries, protocols
3. `ROADMAP.md` — Milestones and statuses (A = Core, B = UI, C = Product)
4. `.agents/rules/` — `code_quality`, `token_discipline`, `execution_loop`
5. `.audit/` — Audit trail log (append-only), one record per work session
6. `CONTEXT.md` (this file) — Rapid orientation + current runtime state

## Stack (Definitively Decided)
Rust 1.85+, Cargo workspace of 9 crates: `core` (agent loop), `llm` (backends + parsers),
`tools` (tools + MCP), `data` (SQLite + FTS5), `proto` (IPC schemas), `svc` (tokio service runtime),
`ui` (wgpu + cosmic-text + M3E), `tui` (terminal client), `app` (entrypoint).
LLM: OpenAI-compatible endpoints (any local or remote endpoints: LM Studio,
Ollama, vLLM, OpenRouter, Gemini, and any compatible models). Embedded llama.cpp is a separate phase later.
License: MIT. Releases: GitHub Releases, Stable + Beta channels. Telemetry: strictly opt-in anonymous counters.

## Current Project Status
- B0 UI Renderer Gate: ✅ PASS — Owner approved GO (prototype `b0.rs`: window, CJK/Cyrillic IME,
  spring morphing; color fidelity fixed via non-sRGB 8-bit format; spacebar handled via `Named(Space)`)
- A0 Skeleton: ✅ | A1 Data Layer: ✅ 5/5 | A2 LLM Adapter: ✅ 20/20 | A3 Loop: ✅ 9/9 | A4 Tools: ✅ 13/13 |
  A5 Permissions: ✅ 14 (10 permissions + 4 diff) | A6 Memory: ✅ 5 | A7 TUI: ✅ 4 |
  A8 Subagents: ✅ (core::subagents + tools::subagents)
- Self-Updater Engine: ✅ (Dual-target user/system atomic replacement, periodic 4m background polling, `/channel` switching, `/update`)
- Workspace Test Suite: 92 passed, 0 failed, `cargo clippy --workspace -- -D warnings` completely clean.
- NEXT MILESTONE: A9 — MCP (client, manager, marketplace registry).
- TUI Polish Notes:
  Parallel streaming of reasoning and content (interleaved reasoning/content deltas fixed);
  settled_boundary = min() across active indices; collapsed reasoning preserves preview line.
  Truecolor ANSI rendering, non-breaking ANSI clipping, composer morphing, F-key shortcuts (F1 Context, F2 Reasoning, F3 Models, F4 Thinking, F5 Sampling, Shift+Tab Mode).

## Verification (Mandatory before closing any milestone)
```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```
UI additionally requires golden frames. Never close a milestone without real command execution output in the audit log.

## Critical Invariants (Do Not Break)
- Crate Boundaries: `core` NEVER knows HTTP, SQL, or UI (abstract traits `LlmSource` / `ToolExec`).
- Injection Protection: tool execution results are always wrapped as `Role::Tool` (untrusted).
- Surface Color Format: use non-sRGB 8-bit (`Bgra8Unorm` / `Rgba8Unorm`) to avoid washed out colors.
- Spacebar Key Handling in winit: `Key::Named(NamedKey::Space)`.
- wgpu 27: `into_static` unavailable; use safe lifetime transmute for Window in App.
- SQLite FTS5 `schema_version`: stored as `TEXT`.
- Token Usage: parsed before choices; text tool scanner is decoupled from HTTP stream.

## Environment
- Agent Sandbox: Headless Linux (x86_64, non-root user).
- Owner System: Windows, PowerShell, multi-lingual.
- API Stream Resilience: on stream disconnects, resume from current position using `.audit/` and `ROADMAP.md`.

## Collaboration Guidelines
- Single focused question batches when soliciting user input.
- Dateless milestones; scope strictly bounded by milestone description ("while we are at it" is forbidden).
- Structured handoffs: Status / Decision / Files / Verification / Open questions / Handoff.
