# Audit: 2026-09-10 TUI Markdown Formatting, Realtime Context Token Gauge, and Recap/Placeholder Polish

### Status: PASS
Decision: Polish TUI markdown rendering (tables, headings, lists), real-time context token usage with tenths precision, and input placeholder/recap persistence.

### Files touched:
- `crates/core/src/context_usage.rs`:
  - Implemented `format_used_tokens(tokens)` with one-tenth-of-a-thousand precision (`{:.1}K`, e.g. `5.2K`, `5.0K`, `8.4K`).
  - Retained clean capacity tokens formatting (`format_capacity_tokens`, e.g. `200K`, `128K`).
  - Updated `format_compact_gauge` to use `format_used_tokens` and `format_capacity_tokens`.
  - Added unit test coverage for tenths precision formatting.
- `crates/tui/src/lib.rs`:
  - Added `tool_result_len: usize` to `ChatLine` and recorded `result_len` on `LoopEvent::ToolFinished`.
  - Added `ChatView::raw_content_chars(&self) -> (usize, usize, usize, usize)` to compute exact character counts for (user, assistant, reasoning, tools).
  - Implemented `render_markdown_text(text: &str, width: usize) -> Vec<String>`:
    - Tables: detects markdown tables (`| col1 | col2 |` + delimiter), measures columns, scales to width, and renders with unicode rounded box-drawing characters (`╭─┬─╮`, `├─┼─┤`, `╰─┴─╯`) and aligned cells.
    - Headings: converts `#`, `##`, `###`, `####` to styled section titles with unicode markers (`◆`, `◈`, `▸`, `▪`), completely removing raw `##` markdown characters.
    - Lists: formats bullet items (`- `, `* `) with clean bullets `•` and hanging indentation on line wrapping; formats numbered items (`1. `, `2. `) with hanging indent.
    - Code blocks: formats fenced code blocks (` ``` `) with border frame (`│`).
  - Integrated `render_markdown_text` into `ChatView::render_split` for assistant responses.
  - Enhanced `reasoning_stage` with Russian keywords and contextual fallback in `push_reasoning` so it never renders bare `Thought for 2s >`.
  - Added unit test `test_render_markdown_tables_headings_and_lists`.
- `crates/tui/src/main.rs`:
  - Updated `update_context_usage` to use `chat.raw_content_chars()` so streaming assistant deltas, reasoning deltas, and tool output immediately update token counts in real time.
  - Fixed input placeholder bug: removed destructive `suggested_prompt = None;` and `custom_placeholder = None;` from `KeyCode::Char` and `KeyCode::Backspace`, ensuring that typing and backspacing restores the suggested prompt placeholder instead of the generic default placeholder.
  - Updated `KeyCode::Esc` so clearing typed text restores the suggestion placeholder.
  - Fixed `build_turn_recap_and_suggestion`:
    - Only treats text as asking questions if `ask_user` tool was called or if the assistant response was short (< 400 chars) and primarily asked a question.
    - Detects project overview requests ("Расскажи о проекте", "объясни", "о проекте") and produces accurate recaps (`"Предоставлен подробный обзор проекта «FlashAgent»"`) and suggestions (`"Архитектура проекта"`).
    - Improved `extract_choices_from_question_text` to parse options from numbered items with colons.
- `crates/llm/src/thinking.rs`:
  - Added "проект", "расскажи", "объясни", "explain", "describe", "overview" to `has_tech_keywords` so exploratory project questions are not classified as `TaskComplexity::Minimal`.

### Verification:
1. `cargo test --workspace`
   - Exit code: 0
   - Output: 171 tests passed; 0 failed; 0 ignored.
2. `cargo clippy --workspace -- -D warnings`
   - Exit code: 0
   - Output: clean, 0 warnings.
3. `cargo build --release`
   - Exit code: 0
   - Output: Built target/release/flashagent in 16.80s.

### Open questions:
None.

### Handoff:
All requested UI rendering, real-time context token updates, input placeholder persistence, and turn recap fixes are in place, tested, and built into the release binary.
