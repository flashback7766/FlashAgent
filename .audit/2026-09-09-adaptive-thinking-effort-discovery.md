# 2026-09-09 — Адаптивная система Thinking/Effort на базе данных API

### Status: PASS
### Decision:
1. **Адаптивная система Thinking/Effort (`crates/llm/src/thinking.rs`)**:
   - Реализована структура `ThinkingProfile` и протоколы `ThinkingProtocol`:
     - `ReasoningEffort` (`reasoning_effort: "..."`)
     - `ReasoningObject` (`reasoning: { effort: "..." }`)
     - `ThinkingObject` (`thinking: { type: "enabled|disabled", budget_tokens: 1024 }`)
     - `BooleanFlag` (`enable_thinking: true|false`)
   - **Динамическое обнаружение пресетов (Discovery)**:
     - Метод `parse_model_metadata`: при старте TUI асинхронно опрашивает эндпоинт `/v1/models` и извлекает поддерживаемые уровни размышлений конкретной модели (`reasoning_effort_levels`, `reasoning_efforts`, `thinking_presets`, `presets`, `enable_thinking`).
     - Метод `parse_api_error`: если при отправке запроса сервер возвращает HTTP 400 со списком разрешённых значений (например, `Value must be one of: ['low', 'high', 'xhigh']` или `expected one of [off, on]`), профиль парсит ошибку, сохраняет найденные пресеты в кеш `profile` и немедленно повторяет запрос с валидным пресетом без прерывания пользовательской сессии.
     - Если бэкенд возвращает ошибку о неизвестном параметре (`unrecognized request argument: reasoning_effort` и т.д.), модель помечается как не поддерживающая размышления (`supported = false`), и последующие запросы отправляются без полей thinking.
   - **Адаптивный подбор усилия (`resolve_effort`, `min_effort`, `max_effort`)**:
     - При необходимости сброса или минимизации размышлений (например, при Auto-Nudge) `min_effort()` автоматически выбирает подходящее значение из обнаруженных у API:
       1. Сначала ищет флаг полного выключения: `off`, `none`, `disabled`, `false`, `0`.
       2. Если отключение не предусмотрено API: выбирает минимальный уровень: `low`, `minimal`, `min`, `fast`.
       3. Если ничего из этого не найдено: выбирает первый доступный пресет из списка API.
     - Для максимального размышления `max_effort()` ищет `xhigh`, `extra-high`, `high`, `max`, `deep`, `on`, либо берёт последний элемент пресетов API.
2. **Интеграция в агентный цикл и TUI**:
   - `OpenAiCompat` получил потокобезопасный профиль `Arc<RwLock<Option<ThinkingProfile>>>` с авто-обновлением.
   - В TUI при инициализации запускается фоновый `llm.fetch_profile()`.
   - В `AgentLoop::run` авто-восстановление после чистого черновика (Auto-Nudge) запрашивает `ThinkingEffort::Low`, который через `ThinkingProfile::resolve_effort` транслируется в реальный минимальный пресет провайдера.

Files touched:
- `crates/llm/src/thinking.rs`: модуль адаптивного обнаружения и маппинга пресетов thinking/reasoning, unit-тесты.
- `crates/llm/src/lib.rs`: экспорт модуля `thinking`, расширение `LlmBackend::stream_with_options`.
- `crates/llm/src/openai.rs`: интеграция `ThinkingProfile`, обработка HTTP 400 с авто-обучением пресетов и повторной отправкой, метод `fetch_profile`.
- `crates/core/src/loop_.rs`: использование `turn_with_options(..., ThinkingEffort::Low)` при Auto-Nudge.
- `crates/tui/src/main.rs`: вызов `llm.fetch_profile()` при инициализации приложения.

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output: 106 passed; 0 failed; 0 ignored; finished in 0.53s.
   (включая `thinking::tests::test_min_and_max_effort_selection`, `thinking::tests::test_parse_model_metadata`, `thinking::tests::test_parse_api_error_extracts_presets`).
2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: Finished `dev` profile [unoptimized + debuginfo] target(s) with 0 warnings.

Open questions:
- Нет.

Handoff:
- Система полностью готова к работе с любыми моделями (OpenAI, Anthropic-compatible, vLLM, Ollama, DeepSeek, OpenRouter), автоматически адаптируясь под их схему и поддерживаемые пресеты reasoning.
