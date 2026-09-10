# Audit: 2026-09-09 TUI Keybinding Robustness, F-Keys, Cyrillic Support, and Slash Commands

### Status: PASS
Decision: C5 (TUI input robustness and hotkey collision resolution)

Files touched:
- `crates/tui/src/lib.rs`:
  - Added `clear()` method to `ChatView` for resetting chat history and streaming states.
  - Updated reasoning suffix to `(f2 · ctrl+o: last · alt+o: all)` and responsive variants.
  - Updated `welcome_card_with_thinking` tip line to highlight `F2`/`Alt+O`, `F4`/`/effort`, `F3`/`/model`, `Shift+Tab`, and `/help`.
- `crates/tui/src/main.rs`:
  - Added collision-free `F2` key to cycle reasoning expansion (`none` -> `last` -> `all` -> `none`) with real-time visible system notices in chat.
  - Added `F3` key to open Model selection menu.
  - Added `F4` key to open Thinking Effort selection menu.
  - Added Cyrillic keyboard layout mappings for all hotkeys:
    - `o`/`O` + `щ`/`Щ` (and `e`/`E` + `у`/`У`) for `Alt+O` (all) and `Ctrl+O` (last)
    - `t`/`T` + `е`/`Е` for `Ctrl+T` / `Alt+T`
    - `m`/`M` + `ь`/`Ь` for `Ctrl+M` / `Alt+M`
    - `c`/`C` + `с`/`С` for `Ctrl+C` exit
  - Added visible status indicator in the input box metadata row: `[reasoning: all]`, `[reasoning: last]`.
  - Added fail-safe prompt slash commands:
    - `/expand` (or `/think`, `/o`) bare or with `all`, `last`, `off`
    - `/effort` (or `/thinking`, `/t`)
    - `/model` (or `/models`, `/m`)
    - `/clear` to clean terminal display
    - `/help` to print command list

Verification:
- `cargo test --workspace` -> Exit code 0 (106 passed; 0 failed)
- `cargo clippy --workspace -- -D warnings` -> Exit code 0
- `cargo build -p flashagent-tui` -> Exit code 0

Open questions:
- None. Key handling is now resilient against IDE global shortcuts, window manager accelerator eating, and Russian keyboard layouts.

Handoff:
- Run `cargo run -p flashagent-tui` and use `F2` or `/expand` to toggle reasoning blocks, `F4` or `/effort` for effort presets, and `F3` or `/model` for models.
