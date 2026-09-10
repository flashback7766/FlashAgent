# UX Audit and Micro-Detail Bug Fixes

### Status: PASS

**Decision:** UX-AUDIT-AND-BUGFIXES

**Files touched:**
- `crates/tui/src/lib.rs`
- `crates/tui/src/wizard.rs`
- `crates/tui/src/main.rs`
- `crates/tui/src/context_modal.rs`
- `crates/tui/src/settings.rs`
- `crates/tui/src/sampling.rs`

**Changes Made:**
1. **Background Recap and Suggestion Retention**:
   - Implemented `ChatView::attach_turn_recap(turn_id: u64, recap_text: &str)` to directly attach recaps to historical turns even if the user has started typing or submitted a subsequent query, preventing recaps from getting lost or dropped.
   - Preserved `latest_suggestion` across prompt typing: cleared placeholder on recap arrival, restored ghost suggestion when the user clears or backspaces typed input back to empty.
   - Extended background recap LLM task timeout to 90s and token limit to 512 to support large local models and high inference load.
2. **Responsive Welcome Card Scaling**:
   - Replaced fixed-height 23-line card with `welcome_card_responsive`, scaling line height and layout based on terminal dimensions (`(term_w, term_h)`).
   - Standard terminals (>= 56 cols, >= 18 rows) use a 14-line 2-column layout.
   - Compact terminals (< 56 cols or < 18 rows) use a 12-line single-column layout with a 3-line pixel companion.
   - Ultra-compact terminals (< 14 rows) use an 8-line layout, ensuring the welcome card never clips the mascot or overflows viewport bounds.
3. **Setup Wizard Input Latency & Paste Support**:
   - Fixed stdin race condition causing ~2s key-holding latency when typing custom backend URLs by routing wizard input via `run_wizard_channel` over the existing TUI `mpsc` channel.
   - Added instant paste support (`Event::Paste`) for Custom Backend URL, API Key, and live Model search fields.
   - Synchronized `config.api_key` immediately on character input, backspace, and paste.
4. **Autonomic Mode Lifecycle Separation**:
   - Clarified and enforced that Autonomic mode is not a persistent selectable `/mode` option, but a temporary mode activated exclusively during `/goal <task>` execution.
   - Replaced `/mode autonomic` in autocomplete popup with standard selectable `/mode edits`.
   - Updated `/mode` parser to explain temporary `/goal` exclusivity if `/mode autonomic` is typed.
5. **Modal Box Width Bounds**:
   - Safely clamped inner modal widths using `.clamp(20, 110)` to prevent panics and border truncation on narrow screens.

**Verification:**
```bash
$ cargo test --workspace
test result: ok. 36 passed in flashagent-tools
test result: ok. 76 passed in flashagent-tui
test result: ok. 7 passed in flashagent-tui (bin)
test result: ok. 5 passed in flashagent-ui
All 124 workspace tests passed with 0 failures (exit code 0).

$ cargo clippy --workspace -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.45s (exit code 0).

$ git grep -I -P "[\x{0400}-\x{04FF}]"
0 Cyrillic characters found in tracked repository files (exit code 1).
```

**Open questions:**
None. All reported regressions and micro-detail UX edge cases are resolved and verified.

**Handoff:**
Repository is in clean passing state on `main` with 2-commit history. All UX improvements and tests verified.
