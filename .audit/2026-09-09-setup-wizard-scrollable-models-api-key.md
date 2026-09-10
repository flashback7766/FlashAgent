# Audit: Scrollable Model List and API Key Stage in Setup Wizard

### Status: PASS
Decision:
1. Implement a 10-item scrollable viewport with live search filtering and PageUp/PageDown support in both `SelectMenu<T>` (`/model` and F3 menu in chat) and `SetupWizard` (Step 2: Model selection) to handle large model catalogues (such as OpenRouter's 430+ models).
2. Insert Step 1 (API key configuration) before Step 2 (Model selection) in the Setup Wizard:
   - For cloud providers (OpenRouter or remote HTTPS endpoints), API key entry is mandatory with instructions on obtaining it (`https://openrouter.ai/keys`).
   - For local providers (LM Studio, Ollama, vLLM, LocalAI, localhost), API key entry is optional and can be left empty by pressing Enter.
   - API key input supports asterisks masking with trailing unmasked chars (`sk-or-••••••••abcd`) and dim placeholder when empty.
   - Advancing from Step 1 to Step 2 uses the provided API key to probe and dynamically fetch the remote models.
3. User clarified via interactive prompt: F3 is preserved for the Model menu (`/model`) and F2 for toggling verbose/reasoning mode.

Files touched:
- `crates/tui/src/select.rs` (`SelectMenu` with 10-item viewport, `filtered_indices()`, `page_up()`, `page_down()`, `push_filter_char()`, `pop_filter_char()`, scroll indicators `▲ ... (N more above) ...` / `▼ ... (N more below) ...`, search indicator)
- `crates/tui/src/wizard.rs` (5-step SetupWizard with mandatory cloud API key / optional local API key, live model filter and paging in Step 2, dynamic probe on transition)
- `crates/tui/src/main.rs` (input forwarding for `SelectMenu` live filtering, PageUp/PageDown, F3 model menu shortcut)

Verification:
1. `cargo test --workspace`
   Exit code: 0
   All 133 tests passed across all crates:
   - flashagent-core: 39 passed
   - flashagent-data: 5 passed
   - flashagent-llm: 24 passed
   - flashagent-tools: 18 passed
   - flashagent-tui: 42 passed
   - flashagent-ui: 5 passed

2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Clean, 0 warnings.

3. Zero emojis:
   Verified 0 emojis across all touched files.

Open questions:
- None.

Handoff:
- Both `/model` (F3) and SetupWizard handle large lists (400+ models) with a smooth 10-item scrolling window and instant search filter. Cloud API keys are strictly validated before model discovery.
