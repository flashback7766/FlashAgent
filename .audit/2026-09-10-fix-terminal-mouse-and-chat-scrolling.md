# Audit: 2026-09-10 Fix Terminal Mouse Capture & Chat History Scrolling

### Status: PASS
Decision: Fixed mouse wheel scrolling and keyboard navigation in alternate screen buffer so mouse scrolling scrolls chat history rather than cycling prompt history in the input composer.

Files touched:
- `crates/tui/src/main.rs`

Verification:
- `cargo test --workspace` -> Exit code 0 (202 passed, 0 failed).
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (0 warnings).
- `cargo build --release` -> Exit code 0 (`target/release/flashagent-tui` and `./flashagent`).

Root Cause:
1. `EnableMouseCapture` was previously removed from terminal initialization in `execute!`.
2. In terminal emulators running in alternate screen buffer mode (`EnterAlternateScreen`) without mouse tracking, the terminal synthesizes mouse wheel movements as Up Arrow (`\x1b[A`) and Down Arrow (`\x1b[B`) key sequences.
3. The input composer caught plain `KeyCode::Up` and `KeyCode::Down` and cycled `input_history`, causing past sent prompts to scroll into the input field instead of scrolling the chat viewport.
4. Additionally, `Renderer::scroll_down` failed to set `needs_reprint = true` unless `scroll_offset` reached 0, causing intermediate downward scrolls to not trigger a redraw.

Changes Made:
1. **Restored Terminal Mouse Capture Lifecycle**:
   - Added `EnableMouseCapture` to terminal setup in `execute!(stdout, EnterAlternateScreen, EnableBracketedPaste, EnableMouseCapture)`.
   - Added `DisableMouseCapture` to panic hook and exit cleanup.
   - Mouse wheel events are now received directly as `MouseEventKind::ScrollUp` and `MouseEventKind::ScrollDown` rather than fake arrow keys.
2. **Fixed Scroll Repaint Trigger in `Renderer::scroll_down`**:
   - `scroll_down` now sets `needs_reprint = true` whenever `old != self.scroll_offset`, ensuring smooth, responsive downward scrolling.
3. **Comprehensive Chat History Navigation**:
   - Added dedicated chat scroll handlers: `PageUp`, `PageDown`, `Home` (scroll to top), `End` (scroll to bottom), `Esc` (return to bottom), `Shift+Up`/`Ctrl+Up`/`Alt+Up` (scroll up 2 lines), and `Shift+Down`/`Ctrl+Down`/`Alt+Down` (scroll down 2 lines).
   - When scrolled up (`renderer.scroll_offset > 0`), plain `Up` and `Down` arrows also scroll chat lines rather than mutating prompt history.
   - Plain `Up`/`Down` only cycle `input_history` when at bottom (`scroll_offset == 0`) and no modifier keys are pressed.
   - Mouse wheel scroll in open menus (`model_menu`, `effort_menu`) navigates menu items.
   - Updated scroll banner hint to: `↑ Scrolled up N lines (↓ / Esc / PageDown / type to return)`.

Open questions: None.
Handoff: Mouse wheel and keyboard scrolling now properly scroll chat history and menus without affecting the input field.
