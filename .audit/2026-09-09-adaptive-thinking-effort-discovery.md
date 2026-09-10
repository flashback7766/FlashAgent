# 2026-09-09 — Adaptive Thinking/Reasoning Effort Discovery & Management

### Status: PASS
### Decision: Adaptive reasoning profile discovery and management across LLM providers

Files touched:
- crates/llm/src/thinking.rs (new: ThinkingProtocol, ThinkingProfile, DiscoveredModel, ServerDiscovery, parse_server_models, parse_api_error)
- crates/llm/src/lib.rs (exported thinking module)
- crates/llm/src/openai.rs (integrated ThinkingProfile into request construction and error feedback loop)
- crates/tui/src/main.rs (dynamic reasoning effort menu populated from discovered API capabilities)

Verification:
- `cargo test --workspace` → passed (llm 34/34 tests green)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings

Design:
- Different APIs use varied protocols for reasoning effort: OpenAI reasoning_effort string, OpenRouter reasoning object, Anthropic thinking object, or boolean flags.
- Instead of hardcoding presets, discovered models inspect server capabilities on `/v1/models` and dynamically adapt.
- On API rejection with supported options list in error body, parser automatically extracts valid presets and updates profile on the fly.

Open questions:
- None.

Handoff:
- Server discovery active in TUI wizard and live turn execution.
