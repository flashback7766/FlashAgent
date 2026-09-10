# 2026-09-09 — Rebinding Alt+O / Ctrl+O & Stage Restructuring

### Status: PASS
### Decision: Clear separation of local vs global reasoning expansion:
1. `Ctrl+O`: temporary toggle for the last turn reasoning block.
2. `Alt+O`: persistent global toggle for all reasoning blocks across session history.
3. Enhanced reasoning stage title normalization for cleaner preview lines.

Files touched:
- crates/tui/src/lib.rs
- crates/tui/src/main.rs

Verification:
- `cargo test --workspace` → passed
- `cargo clippy --workspace -- -D warnings` → 0 warnings

Handoff:
- Keybindings aligned with terminal conventions.
