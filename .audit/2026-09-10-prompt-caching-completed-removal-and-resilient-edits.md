### Status: PASS
Decision: B0 / Prompt Caching Optimization & UI Cleanliness
Files touched:
- crates/tui/src/lib.rs
- crates/tui/src/main.rs
- crates/llm/src/types.rs
- crates/llm/src/parse.rs
- crates/llm/src/openai.rs
- crates/core/src/loop_.rs
- crates/tools/src/fs_tools.rs

Verification:
1. `cargo test --workspace` (Exit code: 0)
   - flashagent_core: 43 passed; 0 failed
   - flashagent_data: 5 passed; 0 failed
   - flashagent_llm: 33 passed; 0 failed
   - flashagent_tools: 34 passed; 0 failed
   - flashagent_tui (lib): 55 passed; 0 failed
   - flashagent_tui (main): 3 passed; 0 failed
   - flashagent_ui: 5 passed; 0 failed
   - Total: 175 passed; 0 failed
2. `cargo clippy --workspace -- -D warnings` (Exit code: 0, 0 warnings)
3. `cargo build --release` (Exit code: 0)

Open questions:
- None.

Handoff:
- `— Completed —` is completely suppressed from chat lines.
- `"cache_prompt": true` and `"prompt_cache": true` are passed to local/OpenAI-compatible backends.
- Assistant messages in multi-turn history preserve `reasoning_content`, guaranteeing LM Studio's Jinja templates reconstruct exact token sequences for `f_keep >= 0.9`.
- `f_keep` ratio is tracked from `prompt_tokens_details.cached_tokens` and displayed live in the TUI status bar (`⚡ cache: XX%`).
- File editing has resilient fallback normalization for CRLF/LF line endings and trailing whitespace.
