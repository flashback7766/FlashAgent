# 2026-09-09 — Shift+Tab Mode Cycling, Arrow Navigation & Reasoning Toggles

### Status: PASS
### Decision: Interactive selector controls and streamlined keyboard shortcuts:
1. **Shift+Tab Mode Cycling**:
   - `PermissionMode` augmented with `next()` and `prev()` methods for circular traversal: `Manual -> Autonomic -> Planning -> Bypass -> Manual`.
   - In TUI, pressing `Shift+Tab` (or `BackTab`) cycles through permission modes with immediate composer update.
2. **Arrow Key Navigation & Approval Selector**:
   - Replaced single-key `a/d` prompts with interactive `ConfirmSelect` component.
   - Left/Right arrow keys toggle between `[Allow]` and `[Deny]`, Enter confirms selection.
   - Up/Down arrow keys in empty composer browse command history.
   - Introduced `select.rs` (`SelectMenu<T>`, `SelectItem<T>`, `ConfirmSelect`) for interactive modal menus.
3. **Reasoning Expansion Scopes**:
   - `Ctrl+O` toggles reasoning for the latest turn.
   - `Alt+O` / `Ctrl+Shift+O` toggles reasoning globally across all turns.

Files touched:
- crates/core/src/permissions.rs
- crates/tui/src/select.rs (new)
- crates/tui/src/lib.rs
- crates/tui/src/main.rs

Verification:
- `cargo test --workspace` → 97 passed; 0 failed
- `cargo clippy --workspace -- -D warnings` → 0 warnings

Handoff:
- Interactive menus and arrow navigation verified in terminal.
