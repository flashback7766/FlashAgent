# Audit: UI Redesign, 500 Tips, Goal Mode & AcceptEdits Permissions

### Status: PASS
Decision:
1. Terminal UI Border Wrap Fix:
   - Root cause identified: wrap() in crates/tui/src/lib.rs measured cur.chars().count() using raw string lengths including ANSI escape sequences (e.g. 19 chars for RGB color codes), causing lines to artificially wrap and split box border characters │ onto subsequent lines.
   - Fixed by measuring visible_width(line) and visible_width(word) in wrap(), ensuring intact preservation of ANSI formatted borders and boxes.
   - Fixed settled line printing in main.rs to prevent redundant markdown parsing (md()) of system lines with pre-formatted escape codes.
2. Information Deduplication & Minimal Welcome Banner:
   - Eliminated redundant info (model, cwd, mode, thinking effort) from the initial welcome card, since these are already rendered in the composer status line.
   - Replaced bulky 12-line welcome card with a clean 2-line banner (>_ FlashAgent (v0.1.0) · M3 Expressive Local Coding Agent).
3. Developer Tips Pool (500 Tips):
   - Implemented crates/tui/src/tips.rs containing exactly 500 practical, emoji-free developer tips covering shortcuts, goal mode, permissions, tools, Rust patterns, Git tricks, Linux shell power commands, and prompt engineering.
   - Tips rotate dynamically above the input composer when idle, disappear automatically during streaming/tool execution, and refresh on turn completion.
4. Permission Modes & Out-of-the-Box Default:
   - Updated PermissionMode enum variants and strict cycling order:
     1. Planning (read-only research)
     2. Manual (asks confirmation for writes and commands)
     3. AcceptEdits (auto-approves file writes/edits, gates shell commands) - default out-of-the-box
     4. Bypass (Accept All)
   - Updated crates/core/src/config.rs default mode to PermissionMode::AcceptEdits.
   - Updated Setup Wizard Step 3 to list and select from the 4 modes with 1-4 shortcuts and default set to Accept Edits.
5. Autonomous /goal <task> Mode:
   - Implemented /goal <task> command: saves active session settings, pre-authorizes actions (PermissionMode::Bypass), sets thinking effort to high, expands step budget to 250, displays dedicated Goal Framing Card, and executes autonomously.
   - Upon turn completion or Ctrl+C interrupt, automatically rolls back permissions, thinking effort, and step budget to their saved state with notification.
6. Setup Wizard Final Step Tool Verification:
   - Final Step 5 in SetupWizard verifies tool execution with list_dir(".") test per PHILOSOPHY.md §10.
7. Terminology Standardization:
   - Purged obsolete "reasoning" terminology across user-facing shortcuts, hints, and help in favor of "verbose" (F2: verbose off/last/all, /verbose).

Files touched:
- crates/core/src/permissions.rs (PermissionMode variants, next(), prev(), label(), decide() logic for AcceptEdits, doc comments, unit tests)
- crates/core/src/config.rs (default permission mode changed to AcceptEdits)
- crates/tui/src/tips.rs (NEW: 500 developer tips and unit tests)
- crates/tui/src/lib.rs (visible_width word wrapping, 2-line welcome banner, tips module export, ChatView::update_welcome_card card_len tracking, unit tests)
- crates/tui/src/wizard.rs (Step 3 permission mode ordering, Accept Edits default, Step 5 list_dir tool verification status, unit tests)
- crates/tui/src/settings.rs (Row 3 permission label display)
- crates/tui/src/main.rs (tip display in composer, /goal command with state save/rollback, mode labels, settled line print fix, command help polish)

Verification:
1. cargo test --workspace
   Exit code: 0
   All 135 unit tests pass:
   - flashagent-core: 40 passed
   - flashagent-data: 5 passed
   - flashagent-llm: 24 passed
   - flashagent-tools: 18 passed
   - flashagent-tui: 43 passed
   - flashagent-ui: 5 passed

2. cargo clippy --workspace -- -D warnings
   Exit code: 0
   Output: Finished dev profile [unoptimized + debuginfo] target(s) in 3.33s
   0 warnings, 0 errors.

3. cargo build --release -p flashagent-tui
   Exit code: 0
   Output: Finished release profile [optimized] target(s) in 9.88s
   Binary /home/flashback/FlashAgent/target/release/flashagent-tui updated. Symlinks ./flashagent and ./flashagent-tui verified pointing to updated binary.

4. Zero emojis check:
   Verified zero emojis across all touched files using Unicode range scanner.

Open questions:
- None.

Handoff:
- UI lines and box borders do not wrap prematurely due to ANSI color codes.
- Welcome card is clean and deduplicated.
- Rotating quick tips render above composer when idle.
- Autonomous /goal command functions with complete rollback upon completion or interrupt.
