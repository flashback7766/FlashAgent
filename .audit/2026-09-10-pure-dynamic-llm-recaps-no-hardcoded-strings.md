# Audit: 2026-09-10 Pure Dynamic LLM Recaps (Zero Hardcoded Strings)

### Status: PASS
Decision: Strict adherence to PHILOSOPHY.md: eliminate all hardcoded recaps or prompt suggestions. Every turn (including greetings) is processed dynamically by the background LLM.

Files touched:
- `crates/tui/src/main.rs`

Verification:
- `cargo test --workspace` -> Exit code 0 (197 tests passed, 0 failed).
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (0 warnings).
- `cargo build --release` -> Exit code 0 (`target/release/flashagent-tui`).

Changes Made:
1. **Removed All Hardcoded Greeting Recaps & Suggestions**:
   - Stripped out the static greeting fallback block completely in `generate_llm_recap_and_suggestion`.
   - All turns — greetings, simple queries, and complex tasks alike — now query the background LLM (`source.turn_with_options`) with the turn's user prompt and assistant response.
   - The model dynamically decides the appropriate recap and imperative user suggestion based on the live dialogue context.
2. **Zero Hardcoded Policy**:
   - Zero fallback strings exist for recaps or suggestions.
   - Recaps and ghost suggestions only appear when dynamically produced by the model.

Open questions: None.
Handoff: Complete dynamic generation without hardcoding across all conversation turns.
