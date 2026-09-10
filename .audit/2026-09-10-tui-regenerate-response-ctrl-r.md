# Audit: Regenerate Last Response on Ctrl+R

### Status: PASS
Decision: Feature Implementation — add ability to regenerate the last model response from scratch via keyboard shortcut `Ctrl+R` (and Cyrillic equivalent `Ctrl+К`), as well as `/regenerate` and `/retry` slash commands.

Files touched:
- `crates/tui/src/lib.rs`: added `ChatView::truncate_to_last_user` method; added unit test `test_chat_view_truncate_to_last_user`; added `Ctrl+R regen` hint to welcome card quick commands box.
- `crates/tui/src/tips.rs`: added tip explaining `Ctrl+R` regeneration in `TIPS_POOL`.
- `crates/tui/src/autocomplete.rs`: added `/regenerate` and `/retry` commands to `builtin_commands`.
- `crates/tui/src/main.rs`: implemented `Ctrl+R` (and `Ctrl+К`) key event handler to truncate history and chat back to the last user message, reset renderer scroll and settled lines, update context usage, and re-spawn turn from scratch; implemented `/regenerate` and `/retry` slash commands; updated `/help` text and Line 1 status bar footer hint.

Verification:
- `cargo test --workspace` (exit code: 0, all 190+ tests passed)
- `cargo clippy --workspace -- -D warnings` (exit code: 0)
- `cargo build --release --bin flashagent-tui` (exit code: 0)
- Live verification in tmux with LM Studio (`gemma-4-e2b-it-qat@q4_k_xl`):
  - Verified `Ctrl+R` on empty chat produces toast "No previous turn to regenerate".
  - Verified `Ctrl+R` on completed turn removes previous assistant answer, reasoning blocks, and recap, and cleanly streams fresh response from scratch.
  - Verified context usage, token tracker, and footer stats (`tg`, `prompt`, `mtp`) update properly for the regenerated turn.

Open questions:
- None.

Handoff:
- Feature is complete, verified, and active in release binary.
