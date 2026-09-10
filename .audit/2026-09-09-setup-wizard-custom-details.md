# Audit: Setup Wizard Custom Endpoint Details Implementation

### Status: PASS
Decision: Apply user preferences chosen via interactive question tool for the Custom endpoint in the Setup Wizard:
1. Auto-prefix `http://` if user enters an endpoint without a scheme (e.g. `localhost:8000/v1` -> `http://localhost:8000/v1`).
2. Maintain block cursor `▌` before the dim placeholder (`▌Enter endpoint here`).
3. Restore and display saved custom URL from `config.json` on subsequent wizard runs instead of clearing it.

Files touched:
- `crates/tui/src/wizard.rs` (added scheme auto-prefix logic on Enter, tested `test_wizard_custom_auto_prefix_http`, tested `test_wizard_preserves_existing_custom_url`)

Verification:
1. `cargo test --workspace`
   Exit code: 0
   All 130 tests passed.

2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   0 warnings, fully clean `#![deny(warnings)]`.

3. Zero emojis:
   0 emojis in source code or UI strings.

Open questions:
- None. All requested behavior implemented and verified.

Handoff:
- The setup wizard custom endpoint UX is complete with auto-prefixed scheme, dim placeholder, block cursor, and state preservation.
