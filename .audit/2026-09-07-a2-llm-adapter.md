# 2026-09-07 — A2: LLM-адаптер

### Status: PASS
### Decision: A2 (LLM-слой)

Files touched:
- Cargo.toml (+reqwest rustls, futures, async-trait)
- crates/llm/Cargo.toml, src/lib.rs, src/types.rs, src/repair.rs, src/parse.rs, src/openai.rs

Verification:
- `cargo test --workspace` → TOTAL passed: 30, failed: 0 (llm: 20/20)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings

Что внутри:
- types.rs: Role/ToolCall/ToolSpec/ChatMessage/Usage/FinishReason/LlmEvent — единые
  протокол-независимые типы; estimate_tokens (фолбэк подсчёта токенов)
- repair.rs: ремонт JSON — умные кавычки, трейлинг-запятые, закрытие строк/скобок,
  дополнение обрезанных литералов (tru→true)
- parse.rs: SseDecoder (сплит-чанки), ChunkParser (текст/reasoning/тулы/usage/finish),
  TextToolScanner (Hermes/Mistral/bare JSON, hold-back частичных маркеров,
  код-фенсы не едятся как тулы)
- openai.rs: LlmBackend trait + OpenAiCompat (LM Studio/Ollama/vLLM/OpenRouter),
  bearer-ключ, таймаут, статусные ошибки

Зафиксированные дизайнерские решения:
- Usage парсится ДО choices и может приходить в любом чанке
- Сканер текстовых тулов пока НЕ встроен в HTTP-поток — подключается в A3 в цикле,
  когда станет ясно, какой бэкенд не отдаёт нативные тулы

Open questions:
- Нет.

Handoff:
- Следующая: A3 — агентный цикл поверх LlmBackend + контрактные тесты на мок-LLM.
