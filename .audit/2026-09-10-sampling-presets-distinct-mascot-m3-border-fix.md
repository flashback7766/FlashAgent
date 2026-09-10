# Audit: 2026-09-10 - Distinct Sampling Presets, Symmetrical Cute Mascot, and M3 Border Junction Fix

### Status: PASS

Decision:
1. Sampling Presets Distinct Calibration:
   - Unified preset buffer synchronization in `SamplingView` (`crates/tui/src/sampling.rs`) with `preset.values()` from `crates/core/src/config.rs`.
   - Removed duplicate hardcoded match in `sampling.rs` that was previously conflating Coding/MtpCoding and Chatting/MtpChatting.
   - Calibrated each preset with distinct, optimal parameters for local & remote LLM architectures (Qwen, DeepSeek, Claude, GPT):
     - Default Coding: Temp 0.60, Top P 0.95, Top K 20, Repeat 1.00, Presence 0.00, Min P 0.00 (matching LM Studio screenshot)
     - MTP Coding: Temp 0.30, Top P 0.90, Top K 40, Repeat 1.00, Presence 0.00, Min P 0.05 (speculative decoding code optimization for high draft acceptance)
     - Multi Token Prediction (MTP): Temp 0.50, Top P 0.92, Top K 30, Repeat 1.02, Presence 0.00, Min P 0.03 (general MTP acceleration)
     - Chatting: Temp 0.80, Top P 0.95, Top K 50, Repeat 1.10, Presence 0.10, Min P 0.00 (conversational, diverse vocabulary, anti-repetition)
     - MTP Chatting: Temp 0.65, Top P 0.88, Top K 40, Repeat 1.05, Presence 0.05, Min P 0.02 (MTP conversational)
     - Precise: Temp 0.10, Top P 0.75, Top K 10, Repeat 1.00, Presence 0.00, Min P 0.00 (strict deterministic logic, math, syntax)
   - Updated descriptive range hints in both `sampling.rs` and `wizard.rs`.

2. Mascot Redesign («Swift» / Companion):
   - Redesigned the 8-bit mascot into a cute, healthy, symmetrical pixel companion:
     - Glowing ice-blue crest `▄█▄` on top
     - Rounded head `▄█████▄` in Material 3 light blue
     - Wings with bright gleaming white circular eyes `●` (`▄██●███●██▄`)
     - Ice-blue beak `▼` centered under solid body (`▀████▼████▀`)
     - Rounded lower belly `▀█████▀` and cute feet `▀   ▀`
   - Perfectly 21-character symmetrical dimensions on every single line with zero deformities.

3. Welcome Box Top Border Junction Color Fix:
   - Fixed top junction `┬─` and corner `╮` in `crates/tui/src/lib.rs` which previously lacked explicit `{M3_BRD}` color codes and were inheriting terminal default yellow/amber text color.
   - All border characters `╭`, `─`, `┬`, `─`, `╮`, `│`, `├`, `┤`, `╰`, `┴`, `╯` now explicitly styled in M3 Slate-Blue (`#4B6382`).

### Files touched:
- `crates/core/src/config.rs`
- `crates/tui/src/sampling.rs`
- `crates/tui/src/wizard.rs`
- `crates/tui/src/lib.rs`

### Verification:
```text
$ cargo test --workspace
test result: ok. 25 passed; 0 failed; 0 ignored (flashagent-llm)
test result: ok. 30 passed; 0 failed; 0 ignored (flashagent-tools)
test result: ok. 46 passed; 0 failed; 0 ignored (flashagent-tui)
test result: ok. 5 passed; 0 failed; 0 ignored (flashagent-ui)
Exit code: 0

$ cargo clippy --workspace -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.15s
Exit code: 0

$ cargo build --release -p flashagent-tui
Finished `release` profile [optimized] target(s) in 13.05s
Exit code: 0
```

### Open questions:
None.

### Handoff:
- Updated binary built and linked at `./flashagent` and `./flashagent-tui`.
- Cycling presets in `/sampling` or `F5` visibly shifts all parameters.
- Mascot is rendered cute, clean, and symmetrical without any yellow junction artifacts.
