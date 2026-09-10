# Audit: Setup Wizard Custom Endpoint Empty State and Dim Placeholder

### Status: PASS
Decision: Configure Custom backend field to be empty by default with a placeholder "Enter endpoint here" rendered 2x dimmer than normal labels (`\x1b[38;2;68;65;62m`). The placeholder vanishes on the very first typed character and reappears whenever the input is emptied. Pressing Enter on empty input prompts for a valid URL instead of advancing.

Files touched:
- `crates/tui/src/wizard.rs` (set `custom_url` initially empty in `SetupWizard::new`, render 2x dimmer placeholder when empty with block cursor when active, validation check on Enter, updated test suite)

Verification:
1. `cargo test --workspace`
   Exit code: 0
   All 128 tests passed (including `test_wizard_custom_backend_selection_and_typing`).

2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Clean, 0 warnings.

3. Zero emojis:
   0 emojis found across the codebase.

Open questions:
- None.

Handoff:
- Option 6 (Custom) in SetupWizard starts empty with the dim "Enter endpoint here" placeholder and disappears/reappears dynamically.
