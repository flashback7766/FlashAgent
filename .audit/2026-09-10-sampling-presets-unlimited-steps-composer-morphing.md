# Audit: 2026-09-10 - Sampling Presets, Unlimited Steps, Menu Border Alignment, Universal Composer Morphing, Persistent Tip Footer

### Status: PASS

Decision:
1. Sampling presets & manual digit input:
   - Added Sampling presets: Default Coding (Temp 0.60, Top P 0.95, Top K 20, Repeat Penalty 1.00, Presence Penalty 0.00, Min P 0.00), MTP Coding, Multi Token Prediction (MTP), Chatting, MTP Chatting, Precise, Custom.
   - Manual digit typing (0..9, ., -) and arrow adjustment (<-/->) auto-switches preset to Custom with auto-clamping to safe ranges.
   - Dedicated interactive Sampling menu accessed via F5, /sampling, /params, and from Settings tab.
2. Unlimited steps per turn:
   - Eliminated the 25-step cap entirely (max_steps: Option<u32> = None by default).
   - Removed all step counters and limits from the user-facing UI.
3. Menu border alignment & cleanup:
   - Fixed border calculation math across menus so top corners align with sides and bottom.
   - Removed Tool Test Status, Web Search, and Max Steps rows from Settings and Wizard Step 5.
4. Universal composer morphing:
   - All interactive dialogs (Setup Wizard, Settings, Model Picker, Effort Menu, Sampling Parameters, Context Modal, Tool Confirmations, ask_user questions) render directly in place of the input box at the bottom.
5. Persistent 3-5 line space under composer:
   - Line 1: Contextual action hints.
   - Line 2: Tip: <content> permanently visible under the composer across all states.
   - Line 3: System status line (model, cwd, mode, thinking) with right-aligned context gauge.
6. ask_user Multi-select:
   - When multi_select: true, options render checkboxes [ ]/[x], Space toggles items, and Enter submits all checked options.

### Files touched:
- crates/core/src/config.rs
- crates/core/src/loop_.rs
- crates/core/src/subagents.rs
- crates/llm/src/types.rs
- crates/llm/src/openai.rs
- crates/tools/src/ask_user.rs
- crates/tools/src/lib.rs
- crates/tui/src/sampling.rs [NEW]
- crates/tui/src/settings.rs
- crates/tui/src/wizard.rs
- crates/tui/src/autocomplete.rs
- crates/tui/src/lib.rs
- crates/tui/src/main.rs

### Verification:
```text
$ cargo test --workspace
test result: ok. 24 passed; 0 failed; 0 ignored (flashagent-llm)
test result: ok. 30 passed; 0 failed; 0 ignored (flashagent-tools)
test result: ok. 45 passed; 0 failed; 0 ignored (flashagent-tui)
test result: ok. 5 passed; 0 failed; 0 ignored (flashagent-ui)
Exit code: 0

$ cargo clippy --workspace -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.01s
Exit code: 0

$ cargo build --release -p flashagent-tui
Finished `release` profile [optimized] target(s) in 13.55s
Exit code: 0

$ ./flashagent --help
FlashAgent TUI
Usage: flashagent-tui [OPTIONS]
Exit code: 0

$ python3 (scan for pictorial emojis)
Zero pictorial emojis found in crates!
```

### Open questions:
None.

### Handoff:
- The compiled release binary is at ./flashagent and ./flashagent-tui.
- Launch ./flashagent to test the unified morphing composer, F5 sampling parameters menu, digit typing, and persistent tip footer.
