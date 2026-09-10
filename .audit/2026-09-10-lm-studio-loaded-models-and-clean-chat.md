# Audit Log: LM Studio Loaded Models Filter & Clean Composer Feedback

### Status: PASS
Decision: UX & LLM Adapter Enhancements (Clean Chat notices & LM Studio loaded-only models with capability summaries)

Files touched:
- `crates/llm/src/thinking.rs`: Added `capabilities_summary(&self) -> String` to `DiscoveredModel` returning formatted context window, tools, vision, and thinking presets/defaults.
- `crates/tui/src/main.rs`:
  - Removed duplicate system notices from chat scrollback for `/compact`, `/mode`, `Shift+Tab`, and `/effort` in favor of rich composer placeholders (`custom_placeholder`).
  - Render frame before async context compaction to show "Compacting conversation context..." placeholder live.
  - Filtered model selection menu (`build_model_menu` / F3 / `/model` / Settings tab) and `available_models` to loaded models (`m.is_loaded`) when LM Studio is the active backend.
- `crates/tui/src/wizard.rs`:
  - Added `discovered_models` tracking and `apply_discovered_models(&mut self, models: Vec<DiscoveredModel>)`.
  - Filtered step 3 model list to loaded models (`m.is_loaded`) if LM Studio is selected.
  - Rendered `● loaded · <capabilities_summary>` beneath model entries and added search matching against capability summaries.
  - Added unit test `test_wizard_lm_studio_loaded_models_filtering_and_capabilities`.

Verification:
- `cargo test --workspace`:
```
test result: ok. 5 passed; 0 failed; 0 ignored (core)
test result: ok. 25 passed; 0 failed; 0 ignored (llm)
test result: ok. 30 passed; 0 failed; 0 ignored (tools)
test result: ok. 52 passed; 0 failed; 0 ignored (tui)
test result: ok. 5 passed; 0 failed; 0 ignored (ui)
Total: 117 tests passed; 0 failed; exit code 0
```
- `cargo clippy --workspace -- -D warnings`:
```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.28s
exit code 0
```

Open questions:
- None.

Handoff:
- Both the wizard and in-app model pickers now automatically filter to loaded models for LM Studio, showing capabilities and configured settings, while chat scrollback remains clean of transient system messages.
