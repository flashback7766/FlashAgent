# Audit: Setup Wizard Custom Backend Endpoint with Manual Keyboard Input

### Status: PASS
Decision: Add option 6 ("Custom") to Step 1 (Choose LLM Backend) of First Start Setup Wizard. When selected (via '6', navigation, or enter), activate direct keyboard typing for the endpoint URL with cursor editing, backspace, delete, and arrow navigation. On Enter, confirm custom URL and advance to model discovery.

Files touched:
- `crates/tui/src/wizard.rs` (added `custom_url`, `custom_cursor`, safe UTF-8 character insertion and deletion, option 6 rendering with block cursor and instruction row, keyboard interception in step 0, and comprehensive unit tests)

Verification:
1. `cargo test --workspace`
   Exit code: 0
   128 passed (including `test_wizard_custom_backend_selection_and_typing`, `test_wizard_custom_navigation_and_arrows`, and `test_wizard_steps_and_navigation`). 0 failed.

2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Clean, 0 warnings.

3. Zero emojis check:
   0 emojis in code, comments, and strings.

Open questions:
- None. Custom endpoint editing is responsive, crash-proof on all UTF-8 boundaries, and works with dynamic server model discovery.

Handoff:
- Option 6 (Custom) is fully active in `SetupWizard`.
- Users can choose presets 1-5 or press 6 / navigate to Custom and type their own host/port/URL directly.
