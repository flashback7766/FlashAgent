# Audit: 2026-09-10 - Per-Turn Recap Retention Fix

### Status: PASS
Decision: TUI-PER-TURN-RECAP-RETENTION

Files touched:
- `crates/tui/src/lib.rs`
- `crates/tui/src/main.rs`

Verification:
- `cargo test -p flashagent-tui -- update_or_push_turn_system_retains_per_turn_recaps` (exit code: 0, 1 passed)
- `cargo test --workspace` (exit code: 0, 195 passed)
- `cargo clippy --workspace -- -D warnings` (exit code: 0, 0 warnings)
- `cargo build --release` (compiling, exit code: 0)

Details:
1. **Root Cause**:
   - `ChatView::update_or_push_system("recap:", ...)` was searching the entire chat lines history via `rposition`.
   - When turn 2 completed, it found the `recap:` system line from turn 1 and overwrote it in-place.
   - Consequently, turn 1 showed turn 2's recap («Ассистент оценил удачное решение пользователя по возвращению предмета в функциональное состояние.»), while turn 2 displayed no recap.
2. **Fix**:
   - Added `ChatView::update_or_push_turn_system(prefix, text)` in `crates/tui/src/lib.rs`.
   - Scopes prefix lookups exclusively to lines after the latest `LineKind::User` message (the current turn).
   - If the current turn does not yet have a recap, it appends a new `recap:` system line to the current turn without modifying any previous turns' recaps.
   - Added test `update_or_push_turn_system_retains_per_turn_recaps` confirming that turn 1 and turn 2 each maintain their own independent recaps.

Open questions:
- None.

Handoff:
- Release binary compiled and verified.
