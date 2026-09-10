# Audit: 2026-09-09 Startup API Discovery, Adaptive Thinking Effort, and Welcome Card Refactor

### Status: PASS
Decision: C5 (Adaptive Thinking, LM Studio v1 REST API, and TUI UX polish)

Files touched:
- `crates/llm/src/thinking.rs`:
  - Added `ThinkingProtocol::LmStudio` for native LM Studio v1 reasoning formatting (`"reasoning": "<val>"` and `"enable_thinking": bool`).
  - Added `DiscoveredModel` struct with context length tracking and humanized `context_display()` (e.g. `128k ctx`).
  - Implemented `parse_server_models()` parsing LM Studio `/api/v1/models` (`models[].capabilities.reasoning.allowed_options`, `loaded_instances[].config.context_length`), `/api/v0/models`, and OpenAI-compatible `/v1/models`.
  - Added `ServerDiscovery` structure to aggregate all discovered models and the currently loaded/active model.
  - Added `default_preset` field to `ThinkingProfile` and updated preset mapping to prevent fallback warnings.
  - Added tests `test_parse_server_models_lm_studio_v1` and `test_parse_api_error_extracts_presets`.
- `crates/llm/src/openai.rs`:
  - Added dynamic model selection via `Arc<RwLock<String>>` with `model()` and `set_model()`.
  - Implemented `discover_server()` probing `{root}/api/v1/models`, `{root}/api/v0/models`, and `{base}/models` on server startup.
  - Automatically activates loaded model instance (`is_loaded: true`) when `--model` is omitted.
  - Updated request body builder to format `ThinkingProtocol::LmStudio` payload correctly.
- `crates/llm/src/lib.rs`:
  - Re-exported `DiscoveredModel` and `ServerDiscovery`.
- `crates/tui/src/lib.rs`:
  - Enhanced `reasoning_stage()` to detect bold section headings (`**Stage Title**`), numbered bold headings (`1.  **Analyze the Request:**`), and markdown headers (`### ...`).
  - Updated `welcome_card()` and `welcome_card_with_thinking()` to render context length (e.g. `model: gemma-4-e2b (128k ctx)`), active thinking preset, directory, mode, loaded memory docs, and updated tips.
  - Updated reasoning summary hints to ` (ctrl+o: last · alt+o: all)` and responsive fallback ` (ctrl+o/alt+o)`.
  - Added unit tests for bold header stage detection and welcome card display.
- `crates/tui/src/main.rs`:
  - Integrated startup server discovery via `backend.discover_server().await` at launch.
  - Added background 10-second ticker to re-check `discover_server()` and notify / sync model, context window, and thinking presets if models are loaded/switched in LM Studio.
  - Added interactive `/model` menu (`SelectMenu`) for runtime model switching.
  - Updated system prompt instructing model to use bold section headers (`**Stage Title**`) for reasoning steps.
  - Wired `ALT+O` to expand/collapse all thinking blocks across turns, and `CTRL+O` to expand/collapse the last turn's thinking block.
  - Fixed crossterm Shift+Tab (`KeyCode::BackTab`) and protected prompt input from control/alt modifier leakage.

Verification:
- `cargo test --workspace` -> Exit code 0 (106 passed; 0 failed)
- `cargo clippy --workspace -- -D warnings` -> Exit code 0
- `cargo build --workspace` -> Exit code 0

Open questions:
- None. All requested features (startup discovery, adaptive effort without fallback warnings, welcome card, bold thinking stages, keybindings) verified.

Handoff:
- Run `cargo run -p flashagent-tui` to launch the interactive TUI against local LM Studio or any OpenAI-compatible server.
