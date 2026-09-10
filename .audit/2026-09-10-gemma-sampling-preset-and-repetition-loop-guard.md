# Audit: 2026-09-10 - Gemma 4 Sampling Preset & Degenerate Repetition Loop Guard

### Status: PASS
Decision: TUI-SAMPLING-PRESET-GEMMA-AND-LOOP-GUARD

Files touched:
- `crates/core/src/config.rs`
- `crates/core/src/loop_.rs`
- `crates/core/src/prompt.rs`
- `crates/tui/src/sampling.rs`
- `crates/tui/src/wizard.rs`

Verification:
- `cargo test --workspace` (exit code: 0, 192 passed)
- `cargo clippy --workspace -- -D warnings` (exit code: 0)
- `cargo build --release --bin flashagent-tui` (exit code: 0)

Details:
1. Added a dedicated `Gemma` sampling preset tailored for Google Gemma 4 models:
   - Temperature: 1.00
   - Top-P: 0.95
   - Top-K: 64
   - Repeat Penalty: 1.08
   - Presence Penalty: 0.02
   - Min-P: 0.03
   This aligns with Google's recommended standard sampling while applying anti-repetition guards essential for quantized (IQ3_S/Q4) MoE weights.
2. Implemented `detect_repetition_loop` and `clean_repetition_loop` in `crates/core/src/loop_.rs`. When an autoregressive loop occurs (e.g. 3 consecutive identical lines or cyclic repeating chunks), FlashAgent immediately breaks streaming execution and prunes the repeated suffix, saving context tokens and preventing 40+ second hangs.
3. Updated `crates/core/src/prompt.rs` with `Direct Tool Invocation` instruction: explicitly forbidding conversational narration of tool calls (e.g. "*Wait, I'll call list_dir.*") to prevent models from generating tool-calling chatter instead of direct tool execution.

Open questions:
- None.

Handoff:
- The release binary has been compiled and verified.
