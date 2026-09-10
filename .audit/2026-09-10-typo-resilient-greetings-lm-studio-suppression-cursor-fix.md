### Status: PASS
Decision: Fix greeting reasoning trigger on typo / trailing key artifacts, suppress thinking tokens in LM Studio payload with 'reasoning_effort: none', and align composer ghost suggestion cursor

Files touched:
- `crates/llm/src/thinking.rs`:
  - Added `pub fn is_greeting_text(prompt: &str) -> bool` with robust word tokenization and layout typo resilience (handling trailing keys from key proximity).
  - Integrated `is_greeting_text` into `ThinkingProfile::analyze_turn_complexity` ensuring `TaskComplexity::Minimal` for all greeting variations while strictly distinguishing coding/task instructions.
  - Updated `ThinkingProfile::apply_to_request` for `ThinkingProtocol::LmStudio` and `ReasoningEffort`: when effort is `off`/disabled, send `reasoning_effort: "none"` alongside boolean flags (`enable_thinking: false`), because LM Studio's `/v1/chat/completions` requires `reasoning_effort: "none"` to suppress `<thought>` generation. When enabled with `on`, map to `reasoning_effort: "high"`.
  - Added model architecture heuristics in `parse_server_models` Check 1 and Check 2 for `gemma-4` and `gemma4`.
  - Added unit tests: `test_is_greeting_text_variants`, `test_lm_studio_apply_to_request_suppression`, and layout typo greeting cases in `test_analyze_turn_complexity`.
- `crates/llm/src/lib.rs`:
  - Re-exported `is_greeting_text` for consumption by `flashagent-tui`.
- `crates/tui/src/main.rs`:
  - Replaced manual greeting matching in `build_turn_recap_and_suggestion` with canonical `flashagent_llm::is_greeting_text`, ensuring proper recap ("Greeted the user") and suggestion ("Explain the project").
  - Adjusted composer input row rendering when `input.is_empty()` to insert space before placeholders and ghost suggestions, preventing the solid block cursor at column 4 from overlapping the first character of the ghost text (e.g. `W` in `What next?`).
  - Added unit test `test_build_turn_recap_and_suggestion_greeting_with_typo`.

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output: 186 passed; 0 failed; 0 ignored; 0 measured across all workspace crates.
2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: Checked all crates with 0 warnings.
3. `cargo build --release --bin flashagent-tui`
   Exit code: 0
   Output: Finished `release` profile [optimized] in 13.80s.
4. Live LM Studio API verification against `gemma-4-e2b-it-qat@q4_k_xl`:
   - Request with `"reasoning_effort": "none"` generated 0 reasoning tokens (`reasoning_tokens: 0`, instant response `"Hello! How can I help?"`).

Open questions:
- None.

Handoff:
- Release binary compiled and verified at `target/release/flashagent-tui`. All greeting typos correctly map to Minimal complexity and thinking suppression. Composer cursor renders cleanly next to ghost suggestions.
