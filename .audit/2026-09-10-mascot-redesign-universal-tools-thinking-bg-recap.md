# Audit: Mascot Redesign, Universal Tools Formatting, Sleek Thinking Blocks, Background LLM Recap & Right-Arrow Ghost Prompt

### Status: PASS
Decision: TUI-A7.2 (Mascot redesign, thinking visualization, universal tool formatting, background LLM recap & right arrow ghost prompt)

Files touched:
- `crates/tui/src/lib.rs`
- `crates/tui/src/main.rs`

### Summary of Changes:
1. **Mascot Redesign («Byte»)**:
   - Replaced pixel-art half-blocks (`▄`, `▀`, `█`) that distorted in terminal fonts with a clean, vector/box-drawing robot companion (`╭───────╮`, `╭┤ ◕   ◕ ├╮`, `╰┤   ‿   ├╯`, `╰┴╯ ╰┴╯`, antenna `⌁o⌁`, hover thrusters `╰┴╯ ╰┴╯`).
   - Symmetrical width: exactly 21 columns on all 6 lines.
   - Built-in blinking animation frames (`^   ^`) on ticks 46 and 47.
2. **Universal Tool Visualization for ALL Tools**:
   - Replaced unparsed JSON (`memory_read({"scope":"project"}) finished >`) with clean, descriptive actions:
     - `memory_read`: `Read memory [project] >` / `Reading memory [project] >`
     - `memory_create`: `Created memory [project] >` / `Creating memory [project] >`
     - `memory_update`: `Updated memory [project] >` / `Updating memory [project] >`
     - `memory_remove`: `Removed memory [project] >` / `Removing memory [project] >`
     - `env_info`: `Inspected environment >` / `Inspecting environment >`
     - `ask_user`: `Asked user: <question> >` / `Asking user: <question> >`
     - Generic fallback for any tool extracts primary arguments (`path`, `target`, `query`, `command`, `task`, `scope`, `key`) and never outputs raw unparsed JSON.
3. **Thinking Blocks Display**:
   - Collapsed view:
     - When stage detected: `Thinking: <stage> >` (streaming) / `Thought: <stage> (Ns) >` (finished).
     - When no stage detected: `Thinking... >` (streaming) / `Thought for Ns >` (finished, e.g. `Thought for 7s >`).
     - Never dumps raw sentences or internal thoughts into the collapsed title.
     - Removed noisy `(f2 · ctrl+o: last · alt+o: all)` shortcut clutter from collapsed lines.
   - Expanded view:
     - Fully enclosed, symmetrical cards (`╭─ ... ─╮`, `│ ... │`, `╰── ... ──╯`) with aligned borders and matching right walls for both thinking blocks and tool call details.
4. **Background LLM Recap & Suggested Reply**:
   - When turn finishes, asynchronously calls `source.turn_with_options` (`ThinkingEffort::Off`, `temperature: 0.3`) in background.
   - Renders 1-sentence recap under assistant response (`recap: ...`).
   - Sets suggested next response in composer placeholder (`(→ to use)`).
5. **Right-Arrow (`→`) Ghost Prompt Insertion**:
   - Changed auto-fill key from Left Arrow to **Right Arrow (`KeyCode::Right`)**.

### Verification:

```text
$ cargo test --workspace
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.43s
     Running unittests src/lib.rs (target/debug/deps/flashagent_core-9a3b9dcb23c32e19)
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running unittests src/lib.rs (target/debug/deps/flashagent_data-994344fa007fc46c)
test result: ok. 10 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s

     Running unittests src/lib.rs (target/debug/deps/flashagent_llm-8b89e67d264563a6)
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.43s

     Running unittests src/lib.rs (target/debug/deps/flashagent_tools-1467f4919eaa8acb)
test result: ok. 30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.35s

     Running unittests src/lib.rs (target/debug/deps/flashagent_tui-af054045d4d98932)
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running unittests src/lib.rs (target/debug/deps/flashagent_ui-85bf50e0ae8e7e67)
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.30s

Exit code: 0

$ cargo clippy --workspace -- -D warnings
    Checking flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.59s
Exit code: 0

$ cargo build --release -p flashagent-tui
   Compiling flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `release` profile [optimized] target(s) in 10.92s
Exit code: 0
```

Open questions:
- None.

Handoff:
- Release binary compiled and verified at `./target/release/flashagent-tui`.
- All features verified across workspace test suite and clippy.
