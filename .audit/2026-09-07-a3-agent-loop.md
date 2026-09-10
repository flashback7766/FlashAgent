# 2026-09-07 — A3: агентный цикл

### Status: PASS
### Decision: A3 (tool loop в crates/core)

Files touched:
- crates/core/Cargo.toml (+flashagent-llm, futures, async-trait, tokio)
- crates/core/src/lib.rs (экспорты)
- crates/core/src/loop_.rs (AgentLoop + 9 контрактных тестов)

Verification:
- `cargo test --workspace` → TOTAL passed: 39, failed: 0 (core: 9/9)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings

Контрактные тесты (мок-LLM, детерминированно):
1. plain_text_completes_in_one_turn — чистый текст, без тулов
2. single_tool_call_executes_and_loops — тул → результат → финальный ответ
3. multi_tool_calls_execute_in_order — параллельные дельты по index, порядок исполнения
4. stream_interruption_fails_loudly — обрыв стрима = LoopError, не молчаливая потеря
5. step_limit_stops_runaway_loop — runaway-модель остановлена лимитом
6. cancel_flag_stops_before_next_turn — Stop кнопка работает между шагами
7. token_budget_trips_when_usage_exceeds — бюджет токенов срабатывает
8. assistant_reasoning_is_kept_in_history — reasoning сохраняется в истории
9. hostile_tool_result_stays_in_tool_role — инъекция остаётся данными Tool-роли

Дизайн:
- Цикл знает только трейты LlmSource/ToolExec — HTTP/SQL/UI не существуют для него
- Все события для UI — LoopEvent (TurnDelta/ReasoningDelta/ToolStarted/ToolFinished/Done)
- DoneReason: Completed/StepLimit/TokenBudget/Cancelled/Failed
- cancel: Arc<AtomicBool>, проверяется перед каждым шагом, внутри стрима и перед каждым тулом

Open questions:
- Нет.

Handoff:
- Следующая: A4 — встроенные тулы (9 штук) + шелл-изоляция, реализация ToolExec.
