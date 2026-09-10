# Audit: UI Scaling, Settings Unification, Wizard Esc & Language Removal

### Status: PASS
Decision:
1. Esc in SetupWizard:
   - Two-stage Esc behavior implemented across all text/search inputs (custom endpoint, API key, model search filter). First Esc clears active text/filter; subsequent Esc navigates back to previous setup step.
   - Pressing Esc on Step 0 cleanly aborts the wizard and exits the application.
2. UI Scaling & Compact Monitor Fixes (1440x810 and narrow terminals):
   - Added `truncate_middle(s: &str, max_len: usize)` helper to preserve version and quantization prefixes/suffixes (e.g. `qwen3.6-35b...preserved-i1`) without overflowing boundaries.
   - Dynamic box width calculation across `welcome_card`, `SettingsTab`, and `SetupWizard` clamping between 44-54 and 110 columns instead of hardcoded 60 or 78/80.
   - ANSI line clipping in `pad_row` closures ensures box borders `│` never wrap or break alignment regardless of terminal size or long content.
   - Normalized `~FlashAgent` directory display to `~/FlashAgent`.
   - Fixed double slash in welcome card hints (`F4 effort · F3 model`).
3. Settings Tab Unification:
   - Settings Tab now serves as the central hub: pressing Enter on LLM Backend launches the interactive `SetupWizard`, pressing Enter on Active Model opens the scrollable `model_menu` (F3), and pressing Enter on Thinking Effort opens the `effort_menu` (F4).
4. Language Removal:
   - Language configuration removed from Settings and Setup Wizard.
   - The model is instructed via the system prompt to always respond in the user's input language.
   - Wizard Step 3 repurposed for Agent Behavior & Permissions (Autonomic, Manual, Planning).

Files touched:
- `crates/tui/src/lib.rs` (`truncate_middle`, dynamic width clamp 44..110 for `welcome_card`, middle truncation of model/cwd/thinking, fixed hint formatting)
- `crates/tui/src/settings.rs` (removed language, added `OpenModelMenu`/`OpenEffortMenu`/`OpenWizard` actions, middle truncation, responsive width 54..110, ANSI row clipping)
- `crates/tui/src/wizard.rs` (two-stage Esc, Step 0 exit, Step 4 Agent Behavior & Permissions, middle truncation in model list, responsive width 54..110, ANSI row clipping)
- `crates/tui/src/main.rs` (wizard abort handling, settings action routing to model/effort/wizard menus, middle truncation in input sub-line, system prompt language rule)

Verification:
1. `cargo test --workspace`
   Exit code: 0
   All 89 unit tests pass:
   - flashagent-llm: 24 passed
   - flashagent-tools: 18 passed
   - flashagent-tui: 42 passed
   - flashagent-ui: 5 passed

2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.51s
   0 warnings, 0 errors.

3. `cargo build --release -p flashagent-tui`
   Exit code: 0
   Output: Finished `release` profile [optimized] target(s) in 7.78s
   Binary `/home/flashback/FlashAgent/target/release/flashagent-tui` updated. Symlinks `./flashagent` and `./flashagent-tui` verified pointing to updated binary.

4. Zero emojis:
   Verified zero emojis across all touched files.

Open questions:
- None.

Handoff:
- UI layout is fully responsive and borders remain intact with long model names or on compact screen resolutions.
- Settings tab directly invokes dedicated sub-menus. Esc in wizard clears search or steps back and exits on step 0.
