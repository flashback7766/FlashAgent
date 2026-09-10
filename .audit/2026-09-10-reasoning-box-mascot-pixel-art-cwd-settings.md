# Audit Record: 2026-09-10-reasoning-box-mascot-pixel-art-cwd-settings

### Status: PASS
Decision: Mascot Pixel Art Precision, Expanded Reasoning Alignment, CWD Normalization & Composer Polish

### Files touched:
- `crates/tui/src/lib.rs`:
  - Fixed expanded reasoning box: replaced clamped 60-char border with `box_w = width.saturating_sub(4).clamp(24, 100)`. Top and bottom borders now have identical character counts down to the exact column. Text wrapping is strictly bounded within `box_w - 4`. Added Material 3 colored headers and bold stage labels.
  - Redesigned mascot sprite to match user pixel art reference `media_1789012304714.png` 1:1:
    - 21-character wide, symmetrical proportions.
    - `#c2e7ff` crest and beak.
    - `#8ab4f8` Material 3 primary blue body.
    - `#1c1b14` dark eye sockets with `#ffffff` round pupils (`●`), transitioning to happy blink eyes (`^`) every 4 seconds.
    - `#1c1b14` dark mouth cavity with `#c2e7ff` downward beak triangle (`▼`).
    - Solid legs `█   █` in `#a8c7fa` positioned at index 8 and 12, directly aligned under the eyes.
  - Added `mascot_swift_lines_animated(tick_n)` and `welcome_card_with_thinking_animated(..., tick_n)`.
  - Normalized `cwd` in welcome card so `~` is always followed by `/` (`~/FlashAgent`).
  - Added regression unit tests for CWD normalization, matching reasoning borders, and mascot dimensions/animation.
- `crates/tui/src/main.rs`:
  - Fixed `cwd_display` relative path stripping to produce `~/FlashAgent` instead of `~FlashAgent`.
  - Added `custom_placeholder` and `suggested_prompt` fields to `FrameState`.
  - Updated composer row to display animated running dots (`Working on task...`), ghost text for suggested prompts with `(← to use)`, and custom placeholder when settings change.
  - Updated settings handlers (`SettingsAction::Close`, `SamplingAction::SaveAndClose`, model menu, effort menu) to update composer placeholder without polluting chat history with system messages.
  - Handled multiple settings changes with `"A whone bunch of settings was changed and i dont know what to say about that......"`.
  - Implemented turn recap upon `UiEvent::Finished` and suggested response auto-fill via Left Arrow (`←`).
  - Added idle mascot blinking animation in tick loop (ticks 46 and 48).

### Verification:
1. `cargo test --workspace`
   - Exit code: 0
   - All tests passed: core, data, llm, proto, svc, tools (30 tests), tui (49 tests), ui (5 tests).
2. `cargo clippy --workspace -- -D warnings`
   - Exit code: 0
   - Zero warnings or errors.
3. `cargo build --release -p flashagent-tui`
   - Exit code: 0
   - Release binary `./target/release/flashagent-tui` built successfully.

### Open questions:
- None.

### Handoff:
- The mascot and UI elements are fully aligned with the user's pixel art and specifications.
