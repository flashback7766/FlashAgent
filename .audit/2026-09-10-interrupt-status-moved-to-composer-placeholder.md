# Session: 2026-09-10 Interrupt Notification Moved to Composer Placeholder

### Status: PASS

### Decision:
Eliminated system chat message clutter on user interruption (`Esc`):
1. **Chat cleanliness**:
   - In `crates/tui/src/main.rs`: Removed `chat.push_system("[Request interrupted by user]")`.
   - In `crates/tui/src/lib.rs`: Suppressed emitting `ChatLine::new(LineKind::System, "— Cancelled —")` in `ChatView::on_event` when `reason == DoneReason::Cancelled`.
2. **Composer placeholder**:
   - In `crates/tui/src/main.rs`: When interrupted via `Esc` or when `UiEvent::Finished` handles `DoneReason::Cancelled`, set `custom_placeholder = Some(interrupt_msg)` (`"Request interrupted by user"`).
   - Displayed directly inside the input prompt field (`❯ Request interrupted by user`) in muted color without polluting conversation scrollback. Clears on user typing or Esc.

### Files touched:
- `crates/tui/src/lib.rs`
- `crates/tui/src/main.rs`

### Verification:
```bash
cargo test --workspace
# Output: 173 passed; 0 failed. Exit code: 0

cargo clippy --workspace -- -D warnings
# Output: 0 warnings. Exit code: 0

cargo build --release
# Output: Finished release profile [optimized] target(s) in 8.33s. Exit code: 0
```

### Open questions:
None.

### Handoff:
Interruption state cleanly renders in the composer placeholder rather than sending system messages to chat history.
