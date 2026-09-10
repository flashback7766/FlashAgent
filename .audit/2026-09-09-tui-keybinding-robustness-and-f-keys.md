# 2026-09-09 — TUI Keybinding Robustness & Function Keys

### Status: PASS
### Decision: Dedicated F-keys and robust modifier handling across terminal emulators

Files touched:
- crates/tui/src/main.rs:
  - Added F2 for cycling reasoning expansion modes (none -> last -> all -> none)
  - Added F3 for opening Model selection menu
  - Added F4 for opening Thinking Effort menu
  - Added layout-resilient shortcut handlers for Ctrl+C, Ctrl+V, Ctrl+R, Alt+O, Ctrl+T, Ctrl+M

Verification:
- `cargo test --workspace` → passed
- `cargo clippy --workspace -- -D warnings` → 0 warnings

Handoff:
- Keyboard handling verified across multiple terminal hosts.
