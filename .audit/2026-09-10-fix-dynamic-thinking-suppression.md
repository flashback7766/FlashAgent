# Audit: 2026-09-10 Fix Dynamic Thinking Suppression on Simple Prompts in Auto Mode

### Status: PASS
Decision: Fix auto reasoning dynamic deactivation for minimal tasks / greetings (LM Studio & local templates)

### Files touched:
- `crates/llm/src/thinking.rs`:
  - Updated `ThinkingProfile::default()` to include `"off"` in presets (`["off", "low", "medium", "high"]`).
  - Updated `min_effort()` so `ThinkingProtocol::BooleanFlag` and `ThinkingProtocol::LmStudio` always return `Some("off")`.
  - Updated `resolve_for_complexity(TaskComplexity::Minimal)` to fallback to `Some("off")`.
  - Updated `apply_to_request` to set `chat_template_kwargs: { "enable_thinking": enabled }` and `chat_template_config: { "enable_thinking": enabled }`.
  - Enhanced `analyze_turn_complexity` with wider conversational greeting & pleasantry detection and short prompt thresholds (up to 50 characters when clean of code/keywords).
  - Added heuristic fallback in `parse_server_models` for Qwen / DeepSeek / Gemma4 models to default to `ThinkingProtocol::LmStudio` with `["off", "on"]`.
- `crates/core/src/prompt.rs`:
  - Updated `build_system_prompt` so when `effort == "off"`, it omits `REASONING INSTRUCTIONS` and injects `THINKING DISABLED: ...`.
  - Removed outdated suggestion encouraging 1-2 reasoning stages for simple greetings.
- `crates/llm/src/openai.rs`:
  - Refactored `body()` to calculate `resolved_effort` upfront.
  - Dynamically adapts the system message for the specific turn: when thinking resolves to `"off"`, prepends `<|think_off|>` (for Jinja templates) and replaces `REASONING INSTRUCTIONS:` with `THINKING DISABLED: ...`.
  - When thinking is enabled, ensures `<|think_on|>` is present for local/LM Studio templates and preserves full reasoning instructions.
  - Added unit test coverage verifying dynamic system prompt mutation and parameter flags.

### Verification:
1. `cargo test --workspace`
   - Exit code: 0
   - Output: 169 tests passed; 0 failed; 0 ignored.
2. `cargo clippy --workspace -- -D warnings`
   - Exit code: 0
   - Output: clean, 0 warnings.
3. `cargo build --release`
   - Exit code: 0
   - Output: Built target/release/flashagent in 11.07s.
4. Live verification against local LM Studio server (`qwen3.6-35b-a3b-uncensored`):
   - Without `<|think_off|>`: 220 reasoning tokens generated.
   - With `<|think_off|>`: 0 reasoning tokens, direct instant response in < 0.3s.

### Open questions:
None.

### Handoff:
Dynamic thinking auto mode now reliably turns reasoning off for simple prompts/greetings across LM Studio and OpenAI-compatible backends.
