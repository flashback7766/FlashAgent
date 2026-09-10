# Audit: 2026-09-10 - 2,000 Unique Tips & Runaway Reasoning Overthinking Guard

### Status: PASS
Decision: TUI-2000-UNIQUE-TIPS-AND-RUNAWAY-REASONING-GUARD

Files touched:
- `crates/tui/src/tips.rs`
- `crates/tui/src/main.rs`
- `crates/core/src/loop_.rs`
- `crates/core/src/prompt.rs`
- `crates/llm/src/thinking.rs`

Verification:
- `cargo test -p flashagent-tui -- test_tips_pool_has_exact_2000_items` (exit code: 0, 1 passed)
- `cargo test --workspace` (exit code: 0, 194 passed)
- `cargo clippy --workspace -- -D warnings` (exit code: 0, 0 warnings)
- `cargo build --release` (exit code: 0)

Details:
1. **2,000 Unique Developer Tips (`crates/tui/src/tips.rs`)**:
   - Cleaned up existing redundant/near-duplicate variations from `TIPS_POOL`.
   - Expanded the pool to exactly 2,000 unique, practical developer tips across Rust stdlib/idioms/memory safety, Cargo commands, modern Git workflows, Linux shell tools, Docker/networking, async Tokio patterns, and systems architecture.
   - Updated unit test `test_tips_pool_has_exact_2000_items` which verifies with a `HashSet` that all 2,000 tips are strictly distinct with zero duplicates.

2. **Runaway Reasoning / 15k Token Overthinking Guard**:
   - In `crates/llm/src/thinking.rs`: changed LM Studio `effort: "on"` mapping to `"medium"` instead of `"high"`. When allowed options are `["off", "on"]`, setting `reasoning_effort: "high"` was forcing Gemma 4 26B into unbounded 16k reasoning mode.
   - In `crates/core/src/prompt.rs`: added `NO RECURSIVE META-ANALYSIS OR OVERTHINKING` instruction: strictly forbidding the model from debating its role, re-evaluating already solved answers, or entering self-doubt loops ("Wait, ...", "Actually, ...", "What if...") once a conclusion is reached.
   - In `crates/core/src/loop_.rs`:
     * Extended `detect_repetition_loop` to detect non-consecutive substantive sentence/clause repetitions (3+ occurrences across paragraphs), immediately stopping runaway reasoning loops.
     * Enhanced `extract_draft_from_steps` to recognize `Response construction:`, `Final decision:`, `Decision:`, `Answer:`, `Response:`.
     * Added a fallback path: if `assistant_text` is empty because the loop broke during reasoning, FlashAgent extracts the already formulated decision from `assistant_reasoning` and outputs it directly to the user (or executes a 1-turn direct answer nudge with `thinking: Off`).

3. **Background Recap & Ghost Suggestion Timeout Resilience (`crates/tui/src/main.rs`)**:
   - Extended background recap generation timeout from 5s to 25s, accommodating larger local models (e.g. 26B Gemma TTFT ~8.35s + generation ~5s).
   - Made `parse_recap_and_suggestion_json` robust against `<thought>`/`<think>` blocks, markdown fences, and conversational wrapping.

Open questions:
- None.

Handoff:
- Release binary built and tested.
