# FlashAgent — Context for AI Agents (Compaction-Safe)

> Purpose of this file: any new agent (or this same agent following context window compaction)
> reads this document to immediately resume work without losing context or intent. Update upon closing every milestone.

## What is This Project
A complete rewrite of flashgent (Electron, legacy in `/home/flashback/flashgent-dev`) into
**`~/FlashAgent`**: a fast, local-first agent amplifier with `/goal` autonomy, a native Rust core +
custom hardware-accelerated wgpu renderer (Material 3 Expressive), zero Electron, zero web views. Open source (MIT).

## Hierarchy of Truth (Read in this strict order)
1. `PHILOSOPHY.md` — The Canon: WHAT and WHY (~110 explicit owner decisions; modify only with explicit instruction from the owner)
2. `ARCHITECTURE.md` — HOW: 8 crates, strict crate boundaries, protocols
3. `ROADMAP.md` — Milestones and statuses (A = Core, B = UI, C = Product)
4. `.agents/rules/` — `code_quality`, `token_discipline`, `execution_loop`
5. `.audit/` — Audit trail log (append-only), one record per work session
6. `CONTEXT.md` (this file) — Rapid orientation + current runtime state

## Stack (Definitively Decided)
Rust 1.85+, Cargo workspace of 8 crates: `core` (agent loop), `llm` (backends + parsers),
`tools` (tools + MCP), `data` (SQLite + FTS5), `proto` (IPC schemas), `svc` (tokio service runtime),
`ui` (wgpu + cosmic-text + M3E, v2), `tui` (terminal client; binary `flashagent`).
LLM: OpenAI-compatible endpoints (any local or remote endpoints: LM Studio,
Ollama, vLLM, OpenRouter, Gemini, and any compatible models). Embedded llama.cpp is a separate phase later.
License: MIT. Releases: GitHub Releases, Stable + Beta channels. Telemetry: strictly opt-in anonymous counters.

## Current Project Status (b233, after the end-to-end audit — see `.audit/2026-09-11-e2e-audit-b233.md`)
- B0 UI Renderer Gate: PASS (prototype `crates/ui/src/bin/b0.rs`; builds and runs its event loop; no golden frames exist yet).
- Track A: A0–A9 closed. A10 `/goal` is PARTIAL: Accept All + max effort + 250-step cap + ask_user/memory
  writes disabled + mode restore. Not implemented: snapshots, milestone commits, token/time budgets, blacklist, live plan, final report.
- Track C: C0 self-updater closed (checksum verification via `SHA256SUMS` added in b233).
- Workspace tests: 284 passed; `cargo clippy --workspace --all-targets --all-features -- -D warnings` clean.
- What the product actually is today: `crates/tui` runs the whole stack in-process. `crates/svc` only holds the
  updater, `crates/proto` is a placeholder, and `crates/data` (SQLite/FTS5) is not used by the
  TUI — sessions are JSON files in `~/.flashagent/sessions/`.
- Text-embedded tool calls (Hermes/Mistral/bare JSON) are executed by the loop since b233 (the parser used to be unwired).
- Releases: build tags `bNNN` / `vX.Y.Z` trigger CI, which republishes the rolling `beta` pre-release or `stable` release.
- NEXT MILESTONE: A10 remaining scope (see ROADMAP), or A11/A12.

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
- Token Usage: parsed before choices; text tool scanner is decoupled from HTTP stream (it runs in `core::loop_`).
- Every assistant tool call in history must be answered by a tool message, also on cancel; only one system message;
  never two user messages in a row (a turn without a reply is closed with an assistant note).
- Tool arguments are read through `flashagent_llm::effective_args` everywhere (permissions, card, preview, tools, MCP).
- Esc/Ctrl+C cancel cooperatively (the loop returns its history); hard abort is a 3 s fallback.

## Environment
- Owner System: CachyOS Linux (Hyprland, fish shell); Windows and macOS are release targets.
- Local runtime checks: LM Studio on `http://localhost:1234/v1` (e.g. `gemma-4-e2b-it-qat@q4_k_xl`).
- API Stream Resilience: on stream disconnects, resume from current position using `.audit/` and `ROADMAP.md`.

## Collaboration Guidelines
- Single focused question batches when soliciting user input.
- Dateless milestones; scope strictly bounded by milestone description ("while we are at it" is forbidden).
- Structured handoffs: Status / Decision / Files / Verification / Open questions / Handoff.
