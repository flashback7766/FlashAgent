# Audit: Setup Wizard, Settings View, Free Search, Touchpad Scroll, Verbose Mode, Context Gauge

### Status: PASS
Decision: First-run Setup Wizard + Settings Tab (`/settings` / `Tab`) + DuckDuckGo Free Search (zero API key) + Touchpad/Trackball Scrolling fix + Verbose Mode (full reasoning & tool payloads) + Context usage gauge & `/context` breakdown modal.

Files touched:
- `crates/core/src/config.rs` (new: `AppConfig`, `BackendPreset`, json load/save `~/.flashagent/config.json`)
- `crates/core/src/context_usage.rs` (new: `ContextUsage`, breakdown accounting, compact gauge formatting)
- `crates/core/src/permissions.rs` (serde derive on `PermissionMode`)
- `crates/core/src/lib.rs` (re-exports `AppConfig`, `BackendPreset`, `ContextUsage`)
- `crates/tools/src/web.rs` (free DuckDuckGo search without API keys, zero external dependency)
- `crates/tools/src/lib.rs` (fallback to free search if `brave_key` is None, always expose `web_search`)
- `crates/tui/src/wizard.rs` (new: interactive `SetupWizard` and `run_wizard` on first run or `--setup`)
- `crates/tui/src/settings.rs` (new: interactive `SettingsView` on `/settings` or `Tab` on empty prompt)
- `crates/tui/src/context_modal.rs` (new: interactive `ContextModal` on `/context`)
- `crates/tui/src/autocomplete.rs` (added `/settings`, `/context`, `/verbose` to slash commands)
- `crates/tui/src/lib.rs` (re-exports, `ChatLine.details`, expanded tool cards in `render_split`, `VerboseMode`)
- `crates/tui/src/main.rs` (EnableMouseCapture/DisableMouseCapture, mouse scrolling event handling, PageUp/PageDown, settings & context modal wiring, context gauge footer)

Verification:
1. `cargo test --workspace`
   Exit code: 0
   All unit and doc tests passed:
   - `flashagent-core`: 39 tests passed
   - `flashagent-data`: 5 tests passed
   - `flashagent-llm`: 24 tests passed
   - `flashagent-tools`: 18 tests passed
   - `flashagent-tui`: 35 tests passed
   - `flashagent-ui`: 5 tests passed
   - Total: 126 tests passed, 0 failed, 0 ignored

2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Zero warnings, fully clean `#![deny(warnings)]`.

Open questions:
- None. All features specified in user requirements are completely implemented and verified.

Handoff:
- The binary is ready to run via `cargo run -p flashagent-tui`.
- On first launch or with `--setup`, it launches the 4-step Setup Wizard.
- Pressing `Tab` on empty input or typing `/settings` opens the interactive Settings Tab with live configuration and auto-saving.
- Touchpad and trackball scrolling work smoothly via `EnableMouseCapture` and the viewport scrollback renderer.
- Running `/context` opens the detailed breakdown modal; the compact gauge is always displayed below the input box on the right.
- Zero-API-key search works out of the box with DuckDuckGo fallback.
