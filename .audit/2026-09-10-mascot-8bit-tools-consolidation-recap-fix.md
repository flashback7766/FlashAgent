# Audit: Authentic 8-bit Mascot «Swift», Memory Tools Consolidation, Expanded Card Header Polish, Semantic Thinking Heuristics, and Immediate Turn Recap with Async Refinement

### Status: PASS
Decision: TUI-A7.3 (Authentic 8-bit mascot «Swift», tool consolidation, clean card headers, semantic reasoning heuristics, deterministic + async turn recap & ghost prompt)

Files touched:
- `crates/tui/src/lib.rs`
- `crates/tui/src/main.rs`

### Summary of Changes:
1. **Authentic 8-Bit Mascot «Swift»**:
   - Implemented a clean, cute, symmetrical 8-bit sprite with block characters (`▄`, `▀`, `█`) based on the reference image (`media_1789012304714.png`).
   - Symmetrical width: exactly 21 character cells on all 6 lines, perfectly centered.
   - Distinctive ice-blue crest (`▄█▄`), symmetrical eye sockets (`●` pupils blinking to `▄` on animation ticks 46-47), beak/mouth with ice-blue triangle (`▼`), and grounded block feet.
   - Normalized working directory path display to correctly show `~/...` when under the user's home folder.
2. **Consecutive Tool Consolidation**:
   - Consecutive calls to `memory_read` (such as `project` and `global` scopes) automatically merge into a single line:
     - Collapsed: `Read memory [project, global] ›` (or `Reading memory [project, global] ›` while running).
     - Expanded (Ctrl+O): `Scope(s): project, global` inside a clean card.
   - Added dedicated styling for `web_search`, `web_fetch`, `git_status`, `git_diff`, and `outline_file` so they never fall back to generic unparsed text.
   - Single file read/search in `format_explore` now specifically mentions the file or query (e.g. `Read src/lib.rs ›` or `Searched needle ›`).
3. **Card Header Polish & Alignment Fix**:
   - Fixed bug where trailing chevron (`>`) remained in expanded tool card headers (`╭─ Read memory [project] > ───`). Trailing chevrons are now stripped using ANSI-aware parsing.
   - Fixed box border misalignment: `vis_len` for lines inside the card is now computed after applying markdown styling (`md(&chunk)`), preventing jagged right walls when text contains `**bold**` or `` `code` `` spans.
   - Top border, body rows, and bottom border mathematically match `box_w` on every line.
   - Transitioned collapsed chevron glyph from ASCII `>` to unicode `›` across all tools and reasoning blocks.
4. **Semantic Thinking Heuristics**:
   - Added intent detection for local models outputting free-form reasoning without explicit bold stage headers:
     - Keyword and semantic heuristics map free-form thoughts to clean intents (e.g. `Reading memory files`, `Checking project rules`, `Formulating response`, `Planning file edits`, `Verifying changes`).
     - Collapsed: `Thought: Reading memory files (6s) ›`.
     - Expanded: `╭─ Thinking: Reading memory files ───────╮`.
5. **Deterministic Instant Turn Recap & Right-Arrow Ghost Prompt**:
   - Turn recap line (`recap: ...`) and ghost prompt (`(→ to use)`) appear immediately on turn completion using deterministic history analysis (`build_turn_recap_and_suggestion`).
   - In the background, `tokio::spawn` queries the LLM for a refined one-sentence summary and 2-4 word user suggestion with a 5-second timeout, updating the recap line in place via `chat.update_or_push_system`.
   - Ghost prompt accepts suggestions into the composer via `KeyCode::Right`.

### Verification:

```text
$ cargo test --workspace
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.87s
     Running unittests src/lib.rs (target/debug/deps/flashagent_core-9a3b9dcb23c32e19)
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/flashagent_data-994344fa007fc46c)
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

     Running unittests src/lib.rs (target/debug/deps/flashagent_llm-8b89e67d264563a6)
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.84s

     Running unittests src/lib.rs (target/debug/deps/flashagent_tools-1467f4919eaa8acb)
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s

     Running unittests src/lib.rs (target/debug/deps/flashagent_tui-af054045d4d98932)
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests src/lib.rs (target/debug/deps/flashagent_ui-85bf50e0ae8e7e67)
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s

Exit code: 0

$ cargo clippy --workspace -- -D warnings
    Checking flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.85s
Exit code: 0

$ cargo build --release -p flashagent-tui
    Finished `release` profile [optimized] target(s) in 5.06s
Exit code: 0
```

Open questions:
- None.

Handoff:
- Release binary compiled and verified at `./target/release/flashagent-tui`.
- All workspace tests passing without warnings or errors.
