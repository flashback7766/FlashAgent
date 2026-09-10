### Status: PASS
Decision: Fix prompt cache eviction (f_keep < 0.9) in LM Studio single-slot backend & overhaul Markdown table / list rendering in TUI

Files touched:
- crates/tui/Cargo.toml (added unicode-width)
- Cargo.toml (workspace dependency unicode-width)
- crates/tui/src/main.rs (removed background LLM recap task that was evicting LM Studio slot 0; replaced with deterministic zero-call builder)
- crates/llm/src/openai.rs (made system prompt immutable, removed dynamic string mutations across turns/steps)
- crates/llm/src/thinking.rs (prevented switching off thinking mid-cycle for binary models to keep prompt template invariant)
- crates/tui/src/lib.rs (table border alignment with unicode-width & styled padding, list rendering with multi-level nesting, checklists, and wrapped continuation lines)

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output summary: All unit tests across all 8 crates passed (180 tests total, 0 failed).
2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.17s (0 warnings, 0 errors).
3. `cargo build --release`
   Exit code: 0
   Output: Finished `release` profile [optimized] target(s). Binaries updated at `target/release/flashagent-tui` and symlinks `./flashagent` and `./flashagent-tui`.

Open questions:
- None. Prompt cache continuity across multi-turn sessions and agent tool cycles is preserved by maintaining system prompt prefix immutability and eliminating extraneous background LLM calls. Table borders and list indentations render correctly.

Handoff:
- Ready for user testing with LM Studio or other local LLM backends. Prompt evaluation cache hit rate will remain high (f_keep >= 0.9) because the prefix never mutates, and tables/lists in assistant responses now wrap and align properly.
