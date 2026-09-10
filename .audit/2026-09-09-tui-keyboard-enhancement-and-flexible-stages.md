# 2026-09-09 — Фикс Ctrl+Shift+O, устранение дублирования мыслей и гибкие маркеры стадий

### Status: PASS
### Decision:
1. **Поддержка Ctrl+Shift+O и универсальные фоллбэки**:
   - В `crates/tui/src/main.rs` включены флаги расширенного терминального протокола `PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)` при входе и `PopKeyboardEnhancementFlags` при выходе.
   - Добавлены универсальные фоллбэки для стандартных эмуляторов терминалов Linux/X11:
     - `Alt+O` (и `Alt+Shift+O`): переключает раскрытие всех блоков размышлений перманентно.
     - Двойное нажатие `Ctrl+O` (в интервале 450 мс): также переключает раскрытие всех блоков размышлений.
     - Одиночное `Ctrl+O`: раскрывает блок размышлений последнего сообщения (временный режим до отправки следующего запроса).
2. **Соблюдение правил 1-5 раскрытия мыслей**:
   - `last_expanded` сбрасывается в `false` при отправке нового пользовательского промпта (`Enter`).
   - `all_expanded` сохраняется между ходами до повторного нажатия хоткея.
   - `render_split` в `crates/tui/src/lib.rs` проверяет принадлежность блоку последнего сообщения через индекс последнего `LineKind::User`.
3. **Устранение двойной генерации размышлений и зависания на черновике**:
   - `crates/core/src/loop_.rs`: внедрены функции `is_pure_thinking_scratchpad` и `extract_draft_from_steps`. Если модель завершила ход без вызова инструментов, но сгенерировала лишь блок черновика/стадий или пустой ответ, агентный цикл автоматически отправляет однократный nudge: `"Please provide your direct, final answer to my request now. Do not repeat the thinking process; output only your final response."` и запускает продолжение генерации.
   - `crates/tui/src/lib.rs`: в `render_split` добавлена дедупликация. Если модель уже прислала нативный `ReasoningDelta`, а затем повторила мысли в `TurnDelta`, повтор подавляется и не засоряет чат вторым раскрытым блоком.
4. **Гибкие маркеры стадий мышления (`[Stage: Название]`, `### Название`)**:
   - Системный промпт в `main.rs` освобождён от навязывания жесткого шаблона из 5 пунктов. Модели разрешено думать свободно, выделяя контрольные фазы маркерами вида `[Stage: Название]` или `### Название`.
   - Функция `reasoning_stage` в `crates/tui/src/lib.rs` обновлена для динамического распознавания `[Stage: ...]`, `[Phase: ...]`, `[Step: ...]`, `### ...`, сохраняя совместимость с нумерованными списками.
5. **Thinking: off/low в API при Auto-Nudge**:
   - В `crates/llm` добавлены `ThinkingEffort` (`Default`, `Off`, `Low`, `Medium`, `High`) и `TurnOptions`.
   - В `OpenAiCompat` внедрена передача `reasoning_effort: "low"` / `reasoning: { effort: "low" }` / `thinking: { type: "enabled", budget_tokens: 1024 }` (и `type: "disabled"` при `Off`) с автоматическим фоллбэком при HTTP 400.
   - В `AgentLoop::run` при срабатывании auto-nudge для хода дозапроса выставляется `turn_opts.thinking = ThinkingEffort::Low`, что предотвращает повторный длинный цикл размышлений у модели.

Files touched:
- `crates/llm/src/types.rs`, `crates/llm/src/lib.rs`, `crates/llm/src/openai.rs`:
  - `ThinkingEffort`, `TurnOptions`.
  - `LlmBackend::stream_with_options` и `OpenAiCompat::body(..., &TurnOptions)`.
  - Фоллбэк при 400 Bad Request на строгих бэкендах.
  - Unit-тест `body_includes_thinking_effort_when_specified`.
- `crates/core/src/loop_.rs`:
  - `LlmSource::turn_with_options`.
  - `AgentLoop::run` с выставлением `turn_opts.thinking = ThinkingEffort::Low` на фазе nudge.
  - `is_pure_thinking_scratchpad`, `extract_draft_from_steps`, `is_reasoning_step_line`.
- `crates/tui/src/main.rs`:
  - Инициализация и сброс `PushKeyboardEnhancementFlags` / `PopKeyboardEnhancementFlags`.
  - `BackendSource::turn_with_options`.
  - Обработка `Ctrl+Shift+O`, `Alt+O` и double-tap `Ctrl+O`.
  - Обновление системного промпта на гибкие маркеры стадий.
- `crates/tui/src/lib.rs`:
  - Поддержка маркеров `[Stage: ...]`, `### ...` в `reasoning_stage` и `is_step_line`.
  - `extract_thinking_from_text` для отделения размышлений и дедупликации.
  - Обновление `render_split` с учётом границы последнего сообщения и дедупликации нативного reasoning.
  - Обновление подсказки Tip в `welcome_card`.
  - Unit-тесты для маркеров стадий, извлечения размышлений и дедупликации.

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output: 98 passed; 0 failed; 0 ignored; finished in 0.52s.
2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: 0 warnings, 0 errors.
3. `cargo build -p flashagent-tui`
   Exit code: 0
   Output: Finished `dev` profile in 7.54s.

Open questions:
- Нет.

Handoff:
- Запустить `cargo run -p flashagent-tui -- --model "<имя_модели>"` и проверить поведение на модели с рассуждениями.
