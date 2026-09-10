# 2026-09-07 — A2: LLM Adapter

### Status: PASS
### Decision: A2 (LLM Layer)

Files touched:
- Cargo.toml (+reqwest rustls, futures, async-trait)
- crates/llm/Cargo.toml, src/lib.rs, src/types.rs, src/repair.rs, src/parse.rs, src/openai.rs

Verification:
- `cargo test --workspace` → TOTAL passed: 30, failed: 0 (llm: 20/20)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings

Contents:
- types.rs: Role/ToolCall/ToolSpec/ChatMessage/Usage/FinishReason/LlmEvent — unified protocol-agnostic types; estimate_tokens (fallback token estimation)
- repair.rs: JSON repair — smart quotes, trailing commas, closing unclosed strings/brackets, repairing truncated literals (tru→true)
- parse.rs: SseDecoder (split chunks), ChunkParser (text/reasoning/tools/usage/finish), TextToolScanner (Hermes/Mistral/bare JSON, hold-back of partial markers, code fences preserved)
- openai.rs: LlmBackend trait + OpenAiCompat (LM Studio/Ollama/vLLM/OpenRouter), bearer key, timeout, HTTP status error handling

Recorded design decisions:
- Usage parsed BEFORE choices and can arrive in any chunk
- Text tool scanner not yet embedded into HTTP stream — wired into A3 loop when backend tool capabilities are negotiated

Open questions:
- None.

Handoff:
- Next: A3 — agent loop on top of LlmBackend + contract tests on mock LLM.
