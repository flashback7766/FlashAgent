# Session: TUI Delta Interleaving Fix (A7.1 Follow-up)

### Status: PASS
### Decision: A7.1 follow-up — fix interleaved reasoning/content deltas; milestone scope only.

Files touched:
- `crates/tui/src/lib.rs` — 3 fixes + new test
- `crates/llm/src/openai.rs` — removed unused import warning in tests

Changes:
1. **Interleaving fix**: `ReasoningDelta` no longer resets `self.streaming`; `TurnDelta` no longer resets `self.streaming_reasoning`. Reasoning and content stream in parallel: one merged Reasoning block + one Assistant line receive appends regardless of delta arrival order.
2. **settled_boundary()**: changed `.or()` chain to `.min()` over all active indices, keeping both live lines safely repaintable.
3. **Collapsed reasoning (ctrl+o)**: every Reasoning line renders as ONE preview row in collapsed mode.
4. **Color and ANSI stability**:
   - `clip_ansi(text, width)`: CSI escape codes copied intact with zero counted width.
   - Truecolor RGB (38;2;r;g;b) used across palette to prevent terminal theme conflicts.
   - `restore_line_color`: reapplies row color prefix after embedded markdown resets.
5. **Stage tracking**:
   - `reasoning_stage(text)` parses labeled steps (`N. Title:`, `* Title:`) to display live stage names in collapsed preview.

Verification:
- `cargo test --workspace` → 81 passed, 0 failed
- `cargo clippy --workspace -- -D warnings` → 0 warnings

Open questions:
- None blocking. Ready for A8.

Handoff:
- Next: A8 subagents per ROADMAP.md.
