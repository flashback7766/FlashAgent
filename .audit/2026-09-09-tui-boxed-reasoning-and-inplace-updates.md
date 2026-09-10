# Audit: 2026-09-09 TUI Boxed Reasoning Container and In-Place Status Updates

### Status: PASS
Decision: C5 (TUI reasoning container redesign and in-place status updates)

Files touched:
- `crates/tui/src/lib.rs`:
  - Implemented boxed container formatting for expanded reasoning blocks:
    - Top border with detected stage title: `╭─ Thinking: <stage> ─────` (or `╭─ Thinking ─────`)
    - Wrapped body text with left vertical border: `│  <markdown_text>` (and `│` for blank lines)
    - Distinct bottom closing border: `╰─────────────────────────────`
  - Added `ChatView::has_user_message() -> bool` to distinguish pre-conversation configuration from active conversation.
  - Added `ChatView::update_or_push_system(prefix: &str, text: &str)` to update system notices in place by prefix matching.
  - Added `ChatView::update_welcome_card(new_card: Vec<RenderLine>)` to re-render the startup welcome card in place before the first user message.
  - Added unit tests: `update_or_push_system_overwrites_matching_prefix`, `update_welcome_card_replaces_card_and_preserves_notices`, and updated `reasoning_deltas_merge_into_one_block`.
- `crates/tui/src/main.rs`:
  - Added `needs_reprint: bool` and `request_reprint(&mut self)` to `Renderer` to force terminal screen clear and repaint on in-place updates.
  - Added `refresh_welcome_card_if_before_user_msg(...)` to dynamically update the welcome card when the model, thinking effort, context window, or execution mode changes.
  - Integrated `update_or_push_system` for all dynamic runtime events:
    - `[Server active model switched: ...]`
    - `[Switched active model to: ...]`
    - `[Thinking effort set to: ...]`
    - `[Reasoning view: ...]`
    - `[Permission mode set to: ...]`

Verification:
- `cargo test --workspace`
  - Exit code 0 (108 passed; 0 failed)
- `cargo clippy --workspace -- -D warnings`
  - Exit code 0 (0 warnings)

Open questions:
- None.

Handoff:
- Launch `cargo run -p flashagent-tui` to interact with the updated TUI.
