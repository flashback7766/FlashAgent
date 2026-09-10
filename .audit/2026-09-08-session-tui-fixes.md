# 2026-09-08 — Session: Render Duplication Fix & TUI Polish (A7.1 Follow-up)

### Status: PARTIAL
### Decision: Three renderer fixes accepted; new delta interleaving issue cataloged for follow-up session.

Files touched:
- crates/tui/src/lib.rs (reasoning preview capped strictly to terminal width)
- crates/tui/src/main.rs (printed_settled: incremental printing of settled lines; HARD CAP on tail lines to prevent terminal wrapping duplication; cursor position clamped to min(width-1))
- CONTEXT.md (cataloged status and fix plan)

Verification:
- cargo test --workspace → 80 passed, 0 failed
- cargo clippy --workspace -- -D warnings → 0 warnings

Root cause of duplicate rendering on screenshot (fixed):
Reasoning preview row exceeded terminal width → physical terminal line wrap → logical tail_height was less than physical lines rendered → cursor-up escape sequences erased fewer lines than printed. Fix: hard-clipping all tail lines to width budget.

Identified issue (delta interleaving):
When local models alternate reasoning and content deltas rapidly, TurnDelta after ReasoningDelta opened a new Assistant row ("word per line"). Planned fix: append into existing Assistant block across interleaved deltas.

Handoff (next session):
1. Fix delta interleaving.
2. Live owner verification: stream, ctrl+o, Esc, approval card, resize.
3. Proceed to A8 subagents per ROADMAP.md.
