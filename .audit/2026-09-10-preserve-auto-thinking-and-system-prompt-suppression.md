# Audit: 2026-09-10 Preserve Auto Thinking & Dynamic System Prompt Suppression

### Status: PASS
Decision: Fix regression where background server discovery and model switching unilaterally overwrote user's `thinking: auto` with model preset `on`, and dynamically adapt system prompt for zero-reasoning turns.

Files touched:
- `crates/tui/src/main.rs`
- `crates/llm/src/openai.rs`

Verification:
- `cargo test --workspace` -> Exit code 0 (197 tests passed, 0 failed).
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (0 warnings).
- `cargo build --release` -> Exit code 0 (`target/release/flashagent-tui`).

Root Causes Identified & Resolved:
1. **Background Discovery Trampled `auto`**:
   - In `crates/tui/src/main.rs` (`UiEvent::ServerDiscovered` and F3 model menu), whenever `model_changed` occurred (e.g. user loaded `gemma-4-26b-a4b-it@iq4_nl` in LM Studio), FlashAgent ran `current_effort = active.thinking.default_preset` (`"on"`).
   - This trampled the default/configured `"auto"` mode into `"on"`, as shown in the user's screenshot (`thinking: on`).
   - In `thinking: on`, every query (even "Hello!") forced the model to think.
   - **Fix**: Updated both `ServerDiscovered` and F3 menu selection to preserve `"auto"` mode if thinking is supported, only setting `"off"` if unsupported.
2. **System Prompt Adaptation for Zero-Reasoning Turns**:
   - When thinking is resolved to `"off"` (such as for greetings in `auto` mode), in addition to setting API flags (`enable_thinking: false`, `reasoning: "off"`, `reasoning_effort: "none"`), the system prompt now dynamically replaces `REASONING INSTRUCTIONS:` with `<|think_off|>THINKING DISABLED: ... Answer directly.`
   - This ensures models never generate thought headers even if chat templates rely on prompt tokens.

Open questions: None.
Handoff: `thinking: auto` remains persistent and reliably suppresses reasoning on greetings and pleasantries.
