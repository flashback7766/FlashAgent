# Audit: Real-Time Model-Generated Turn Recaps & Contextual Suggestions

### Status: PASS
Decision: Feature Enhancement — Real-time model-generated turn recaps and contextual composer suggestions in FlashAgent TUI, complete elimination of static placeholder templates, and robust background query pipeline with LM Studio / local inference compatibility.

Files touched:
- `crates/llm/src/types.rs`: added `pub max_tokens: Option<u32>` to `TurnOptions`.
- `crates/llm/src/openai.rs`: wired `options.max_tokens` into the OpenAI HTTP request payload (`body["max_tokens"] = serde_json::json!(m)`).
- `crates/tui/src/main.rs`:
  - Added `UiEvent::BackgroundRecap { turn_id: u64, recap: String, suggestion: Option<String> }` to `UiEvent`.
  - Added `turn_counter: u64` in main loop, incremented across all turn start triggers (`Ctrl+R`, `/goal`, `/regenerate`, `/skill:`, prompt submit).
  - Updated `build_turn_recap_and_suggestion` to return `(String, Option<String>)`:
    - Handles prompts like "What's next?" with `"Action plan proposed"` and step 1 extraction.
    - Replaced robotic `"Detailed response provided for query..."` with natural `"Answered question..."`.
    - Eliminated `"What's next?"` from all fallback branches; returns `None` for clean neutral placeholder.
  - Implemented `generate_llm_recap_and_suggestion`:
    - Spawns background task upon `UiEvent::Finished(Ok((h, reason)))` with `ThinkingEffort::Off`, `max_tokens: 80`, `temperature: 0.3`, and 5-second timeout.
    - Prompts model for concise 1-sentence recap and 2-4 word contextual user prompt suggestion.
    - Robust JSON parsing, markdown fence stripping, and safety filters for generic filler.
  - Handled `UiEvent::BackgroundRecap`:
    - Validates `turn_id == turn_counter && active_turn_handle.is_none()`.
    - Updates recap line in-place in chat view (`chat.update_or_push_system("recap:", ...)`).
    - Dynamically updates composer ghost suggestion when `input.is_empty()`.
    - Calls `renderer.request_reprint()`.
  - Updated and added unit tests in `main.rs` for `Option<String>` suggestions, placeholder elimination, and JSON parsing.

Verification:
- `cargo test --workspace` (exit code: 0, all 193 tests passed)
- `cargo clippy --workspace -- -D warnings` (exit code: 0, 0 warnings)
- `cargo build --release -p flashagent-tui` (exit code: 0)
- Live interactive test in tmux with LM Studio (`gemma-4-e2b-it-qat@q4_k_xl`):
  - Query 1: *"What is Swift?"*
    - Recap updated in-place to: *"Swift is a modern multiplatform programming language from Apple for building applications."*
    - Suggestion displayed in composer: *"Learn Swift features? (→ to use)"*
  - Pressed `Right` arrow (→): filled input with *"Learn Swift features?"*.
  - Query 2: *"Learn Swift features?"*
    - Recap updated in-place to: *"Explained that Swift is a modern multiplatform language with emphasis on safety and static typing."*
    - Suggestion displayed in composer: *"Show code examples? (→ to use)"*
  - Verified 0 occurrences of placeholder loops and 0 occurrences of robotic templates.

Open questions:
- None.

Handoff:
- Feature is complete, verified end-to-end with live local model inference, and active in release binary.
