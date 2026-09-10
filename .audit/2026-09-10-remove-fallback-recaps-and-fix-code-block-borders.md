# Audit: 2026-09-10 - Remove Fallback Recaps and Fix Code Block Right Borders

### Status: PASS
Decision: TUI-NO-FALLBACKS-AND-CODE-BLOCK-BORDERS

Files touched:
- `crates/tui/src/main.rs`
- `crates/tui/src/lib.rs`

Verification:
- `cargo test --workspace` (exit code: 0, 189 passed)
- `cargo clippy --workspace -- -D warnings` (exit code: 0)
- `cargo build --release --bin flashagent-tui` (exit code: 0)

Details:
1. Removed all hardcoded heuristic fallback strings and functions (`build_turn_recap_and_suggestion`, `extract_choices_from_question_text`, `extract_task_summary`, and 260 lines of static templates).
2. Recaps and ghost suggestions are now ONLY displayed if and when genuinely returned by the model via background LLM generation. When absent or failed, no fake recap lines or default placeholder suggestions are pushed.
3. Fixed missing right border `│` in markdown code block rendering (`render_markdown_text` in `crates/tui/src/lib.rs`). Previously, code lines inside ```` blocks only had a left border `{border_color}│{reset} `, leaving the box open on the right. Now each line inside the block is enclosed with a right vertical border `│` and padded to the exact same width as the top (`╭─...╮`) and bottom (`╰─...╯`) borders.
4. Added unit test `test_render_markdown_code_block_exact_border_alignment` verifying that every single line of a code block has matching visible width and a closed right border.

Open questions:
- None.

Handoff:
- Workspace clean, tests pass, release binary compiled.
