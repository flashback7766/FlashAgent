# FlashAgent — Philosophy Manifesto

> Canonical document. All future agents and developers must adhere to it.
> Highest authority rule: this file defines WHAT and WHY; ARCHITECTURE.md defines HOW; ROADMAP.md defines IN WHAT ORDER.
>
> **v1 is the terminal app; the native UI is v2** (owner decision, 2026-09-11, amended here 2026-09-22). Where this file describes the native renderer, its processes and its storage, it describes v2. The terminal app follows the same principles wherever a terminal can: full composer editing, a master motion toggle, the status ribbon, keyboard coverage.

## 1. Product Essence

FlashAgent is a **local agent-amplifier**: human leads, agent accelerates.
A single `/goal` command transforms it into a **fully autonomous agent** bounded by the task envelope.

- Default: amplified interactive chat with tools.
- `/goal`: autonomous — adaptive execution layers built over a unified core (simple task → reactive loop; complex task → plan → milestones → self-verification → structured summary).
- Four permission modes: Planning (read-only) / Manual (ask before anything that is not a read) / Accept Edits (file changes allowed, shell asks) / Accept All.

## 2. Unacceptable (Anti-Philosophy)

Eliminated by design:
1. **Feature bloat** — a feature exists only if it directly serves the core loop: "human → task → result".
2. **Flaky tool calling** — calling a tool must never be a lottery.
3. **Electron** — dead. No web views, no bundled Chromium.
4. **Crude UX details** — every micro-interaction refined to Material 3 Expressive standards.

## 3. Locality and Network

Inference and data stay local by default. The network operations FlashAgent makes:
- OTA updates: GitHub Releases, checked, downloaded and installed in the background (checksum-verified), used from the next launch; can be turned off in Settings.
- MCP servers configured by the user (confirmation required with argument preview for non-read-only actions).
- Telemetry: planned, opt-in; none today. When it exists: a launch counter, version, OS, active backends/tools, zero session content; crash reports opt-in, sanitized stack traces only, no memory dumps.
- Agent web tools (`web_fetch`/`web_search`): on by default and treated like reading a file, in every permission mode; they can be turned off in Settings.

## 4. Technology Stack (Definitively Decided)

| Layer | Decision |
|---|---|
| Core | **Rust** (tokio, serde) |
| UI | v1: **terminal UI** (crossterm), the product itself. v2: **custom wgpu renderer** — design takes precedence over generic frameworks |
| Text (v2) | **cosmic-text / parley** — font shaping, CJK, RTL, emoji, ligatures |
| Theme | **Material 3 Expressive**: v1 in the terminal's colours (with themes for 16-colour and monochrome terminals); v2 in full, every button can expand into a mini-panel, expressive palette, iconography |
| Fonts (v2) | Bundled: proportional (Roboto Flex/Inter) + monospace (JetBrains Mono) + user TTF/OTF loading. Emoji: embedded Noto Emoji |
| Icons (v2) | **Material Symbols** |
| Data | v1: **plain files** under `~/.flashagent/` (JSON sessions written atomically, snapshots, config). v2: **SQLite + FTS5**, for the session sidebar's search |
| Processes | v1: **one process**. v2: **UI process + core service** via IPC; a UI crash never interrupts the active task |
| Inference | HTTP backends currently (any OpenAI-compatible endpoint: LM Studio, Ollama, vLLM, OpenRouter, Gemini, and any other model without exception); **embedded llama.cpp — separate milestone after core app runs flawlessly**; paired with grammar/constrained generation for tool calling |
| TUI | v1 is the terminal app. In v2 it stays: a client of the same core, for terminal users and for automated testing |
| License | **MIT** |
| Documentation | Repository-generated site (mdBook on GitHub Pages); concise entrypoint in README |
| Releases | Stable + Beta channels: betas are numbered builds (`b<N>`), stable releases are SemVer cut from a build (`vX.Y.Z+b<N>`); see [VERSIONING.md](VERSIONING.md) |
| i18n | The interface and the repository are English only; the model always answers in the language the user wrote in |

## 5. Tool Calling — Reliability is Sacred

- Unified internal `ToolCall` data structure + **protocol adapters**: native JSON tool calls, XML tags, NDJSON blocks. The same single-line JSON format is parsed across all ingress channels.
- Multi-parser hardened and resilient right now; formal grammar sampling integrated when llama.cpp is embedded.
- Contract test suite: mock LLM server testing edge scenarios (stream truncation, malformed JSON, prompt injections, unclosed blocks). Deterministic, zero reliance on live models.

## 6. Permission Model

- Modes: Planning / Manual / Accept Edits / Accept All; Shift+Tab cycles them; `/goal` runs in Accept All inside explicit boundaries and restores the mode after.
- Confirmations: Allow / Always / Deny interactive card in stream; Always uses narrow prefix rules (`npm test` ≠ `npm publish`); session-scoped rules; **mandatory diff preview prior to file modifications**.
- Subagent permissions: strictly inherited from parent, **privilege escalation is architecturally forbidden**.

## 7. Autonomous Execution (/goal)

- Task envelope: step count limit, token budget, execution timeout (all set in Settings, unlimited by default), action blacklist (refused outright during `/goal`). A question the agent asks waits two minutes, then the run carries on with its own best choice.
- Reporting: live plan document in UI + final structured report (completed / omitted / manual verification required).
- File safety: pre-task filesystem snapshot + milestone git commits.
- Subagents: fully dynamic orchestration — dynamic roles, concurrency throttling, parent review gate, inter-agent messaging; UI: interactive tree with live cards, subagent tabs, and overlay HUD.

## 8. Memory

- Project memory (`MEMORY.md` in repository root) + global memory (`~/.flashagent/MEMORY.md`).
- **Auto-discovery** of external rule files: `MEMORY.md`, `CLAUDE.md`, `AGENTS.md`.
- Memory modifications require user confirmation (automatic in autonomic mode).
- Context injection: injected whole when budget allows; otherwise outlines with targeted tool inspection.
- UI: memory editor, dual-tier view, change diffs, indicator for detected rule files.

## 9. Toolset

Adaptive: **auto-selected based on the model's context window** (ultra-minimal ~32k, core ~64k, expanded on large context models). User MCP servers provide additional tools.

## 10. UI Philosophy

This section describes the v2 native UI in full. The v1 terminal app keeps
every point a terminal can express and says so in ROADMAP when it cannot.

- Design: **M3 Expressive, implemented literally** — custom wgpu renderer.
- Animations: **rich visual feedback**, all four classes: spring physics, continuous ambient background loops (subtle, non-distracting), state morphs (Stop ↔ Send, diff expansion), master disable toggle.
- Fully animated, looped, and parallel rendering.
- Adaptive density (compact / comfortable).
- Tool stream: live peek + sequential batching + **Verbose** mode (thoughts in muted grey, full-width non-collapsible code blocks) and **Transcript** mode (same, with standard tool cards).
- Scrolling: anchor auto-scroll, smooth physics, scroll position retention.
- Composer: auto-expanding with height cap + full editing support.
- Agent status: single ticker ribbon (current action, tokens, generation speed, context usage).
- Keyboard: complete shortcut coverage; mouse and touchscreen equally ergonomic.
- Error handling: inline error card with Retry action + dedicated diagnostics pane.
- Subagent visualization: live card tree, dedicated tabs, counter with overlay.
- Sessions: sidebar with instant FTS5 search.
- MCP: settings manager + integrated marketplace + confirmation preview cards.
- Diffs: unified renderer + split view + line-by-line selection + live diffing.
- Streaming: live markdown rendering on the fly.
- Reasoning models: first-class support (`reasoning_content`/`thinking`), rendered muted grey in stream, full in Transcript.
- Tokens: accurate counters from backend API, local fallback via tiktoken estimation.
- First launch: **onboarding wizard** (backend selection, connection check, model verification, test tool call).

## 11. Performance Requirements

Measured figures for the terminal app are in [docs/numbers.md](docs/numbers.md). The display requirements (refresh rate, DPI, AccessKit) are for v2.

- Cold start < 1s.
- FPS = monitor refresh rate (native Hz, full VRR / LTPO / live display hotplug support); zero frames rendered without changes (sleep frames).
- Fractional DPI scaling (125%, 150%) with subpixel sharpness; live display switching.
- Massive sessions (100k+ messages) scroll without dropped frames.
- RAM: noticeably lighter than Electron (~100-200 MB without model), soft target, non-blocking.
- IME (Cyrillic, CJK) from day one. AccessKit accessibility from day one.

## 12. Rewrite Strategy

- Old flashgent (Electron): **frozen**, preserved partially as legacy reference. Source of battle-tested components (parsers, safety guards).
- New repository: `flashagent`. Product brand: **FlashAgent**.
- Roadmap strategy: originally **two parallel tracks** — core engine (validated via TUI) and UI renderer prototype. The prototype passed its gate (B0); on 2026-09-11 the owner decided v1 ships the terminal app and the native UI resumes after v1.0.
- Rewrite bottom-up layer by layer.
- Data migration from `~/.flashgent`: closed without work (ROADMAP A12), there was no data to import.
- v1 milestone criteria: (1) author switches to daily production use, (2) public release polished sufficiently to attract active community adoption, (3) thriving ecosystem (GitHub stars, third-party MCPs and themes).
- Cadence: milestones without rigid calendar deadlines.

## 13. Agent Instructions Post-Manifesto

1. Manifesto (this file) → 2. ARCHITECTURE.md + ROADMAP.md (detailed specifications ready for autonomous handoff) with **overhauled skills system and structured project audit** → 3. Code: core engine + UI prototype tracks, with owner verification at every step.

## 14. Appendix: Complete Registry of Decisions

Derived from the architectural inquiry (9 categories, ~110 explicit decisions), summarized fully above. In case of conflict: this document takes precedence over agent memory.
