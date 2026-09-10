# Audit: 2026-09-10 - Imperative User Ghost Suggestions and Thinking Suppression

### Status: PASS
Decision: TUI-GHOST-SUGGESTIONS-IMPERATIVE

Files touched:
- `crates/llm/src/openai.rs`
- `crates/tui/src/main.rs`

Verification:
- `cargo test --workspace` (exit code: 0, 194 passed)
- `cargo clippy --workspace -- -D warnings` (exit code: 0)
- `cargo build --release --bin flashagent-tui` (exit code: 0)

Details:
1. Sanitized ghost suggestions so that assistant inquiry phrasing and infinitive forms ("would you like to learn more about...", "tell about...", "show examples...") are converted into direct user imperative prompts ready to send:
   - "Explain Swift features"
   - "Show Swift code examples"
   - "Explain memory layout in Swift"
   - Trailing punctuation (`?`, `.`, `!`) is stripped so suggestions format as actionable commands.
2. In `crates/llm/src/openai.rs`, added thinking suppression for local / LM Studio endpoints even when `ThinkingProfile` is uninitialized or unsupported by default, ensuring models like Gemma 4 do not waste their 120-token completion budget on internal reasoning.
3. Added unit tests in `crates/tui/src/main.rs` verifying sanitization, conversion of questions/infinitives into imperatives, and rejection of generic filler words.

Open questions:
- None.

Handoff:
- The release binary has been compiled and all 194 tests pass cleanly.
