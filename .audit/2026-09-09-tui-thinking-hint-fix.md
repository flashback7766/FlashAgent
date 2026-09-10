# 2026-09-09 — Thinking Preview Hint Localization & Persistence Fix

### Status: PASS
### Decision: Polish reasoning preview rendering:
1. **Localization**: Replaced hardcoded preview string with consistent `(ctrl+o to expand)`.
2. **Hint Persistence**: Preserved `(ctrl+o to expand)` hint on collapsed reasoning blocks even after turn completion (previously stripped on `Done` event).
3. **Width Budget**: Stage title uses available terminal width without artificial 50% clipping.

Files touched:
- crates/tui/src/lib.rs:
  - Updated `render_split` reasoning suffix to `(ctrl+o to expand)` permanently.
  - Adjusted stage label budget to fit terminal width.
  - Added unit test validation in `collapsed_preview_shows_current_stage`.

Verification:
- `cargo test --workspace` → 93 passed; 0 failed
- `cargo clippy --workspace -- -D warnings` → 0 warnings

Handoff:
- Verified clean display across turn transitions.
