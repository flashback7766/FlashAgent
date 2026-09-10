# Audit: 2026-09-10 Instant Greeting Recap & Prompt Suggestion

### Status: PASS
Decision: Fix missing recap and ghost suggestion on greetings and initial turns.

Files touched:
- `crates/tui/src/main.rs`

Verification:
- `cargo test --workspace` -> Exit code 0 (198 tests passed, 0 failed).
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (0 warnings).
- `cargo build --release` -> Exit code 0 (`target/release/flashagent-tui`).

Root Cause Identified & Resolved:
- In `crates/tui/src/main.rs` (`generate_llm_recap_and_suggestion`), the function had an early return:
  ```rust
  if flashagent_llm::is_greeting_text(user_prompt) {
      return None;
  }
  ```
  When the user sent "Привет!" or any conversational greeting, this check caused the function to immediately abort and return `None`. Consequently, no `recap:` block was added to the turn, and `suggested_prompt` remained `None`, leaving the input box with the empty default placeholder.
- **Fix**:
  1. Updated `generate_llm_recap_and_suggestion` so that greetings immediately receive an instant, zero-latency conversational recap ("Ассистент поприветствовал пользователя и готов к работе по проекту") and starter ghost suggestion ("Покажи структуру проекта").
  2. Bumped `max_tokens` for the background LLM recap query from 120 to 256 to ensure complex Russian JSON completions never get truncated before the closing curly brace.
  3. Added unit test `test_greeting_recap_and_suggestion_instant` confirming immediate recap and suggestion return.

Open questions: None.
Handoff: Recaps and suggestions now render reliably on every turn, including greetings.
