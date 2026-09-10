# 2026-09-09 — Перебинд Alt+O / Ctrl+O, структурирование мышления по стадиям и TUI Thinking Effort

### Status: PASS
### Decision:
1. **Переназначение горячих клавиш (Rebind)**:
   - `ALT + O`: перманентное раскрытие / сворачивание всех блоков мыслей (`all_expanded = !all_expanded`).
   - `CTRL + O`: временное раскрытие / сворачивание блока мыслей последнего ответа (`last_expanded = !last_expanded`).
   - Устранены задержки double-tap и отброшен конфликтный `Ctrl+Shift+O`, который терминалы Linux эмулировали как `0x0F` без отличия от `Ctrl+O`.
   - Обновлены подсказки в футере TUI: `alt+o — all · ctrl+o — last · ctrl+t — effort`.
   - Обновлена строка `Tip` в приветственной карточке `welcome_card`.
2. **Структурирование стадий мышления модели**:
   - Обновлён системный промпт в `crates/tui/src/main.rs`: модель строго инструктируется начинать каждую фазу размышлений с явных маркеров стадий: `[Stage: Understand Request]`, `[Stage: Analyze Context]`, `[Stage: Plan Steps]`, `[Stage: Formulate Response]` (или нумерованных шагов `1. Understand Request:`).
   - В свёрнутом виде TUI отображает текущую стадию: `↳ Thinking: Understand Request (ctrl+o to expand)`, позволяя отслеживать ход мыслей модели в реальном времени.
3. **Реализация выбора и отображения Thinking / Effort в TUI**:
   - Введена строка статуса внутри инпут-бокса: `gemma-4... · ~FlashAgent · [Mode] · thinking: <preset>`.
   - В приветственную карточку `welcome_card_with_thinking` добавлен вывод текущего пресета и списка доступных у модели: `thinking: high [low, high]`.
   - Добавлено интерактивное меню выбора `SelectMenu<String>` по горячей клавише `Ctrl+T` или `Alt+T` (а также по командам `/effort` или `/thinking` в инпуте).
   - Меню наполняется динамически из найденных у API пресетов (`ThinkingProfile::presets`), управляется стрелками `↑ / ↓`, выбирается `Enter` и отменяется `Esc`.
   - Выбранное значение передаётся в `AgentLoop` через `LoopConfig.base_turn_options` и пробрасывается в запросы к модели.

Files touched:
- `crates/llm/src/types.rs`: поле `custom_effort` в `TurnOptions`.
- `crates/llm/src/openai.rs`: применение `options.custom_effort` при формировании тела запроса; unit-тесты.
- `crates/core/src/loop_.rs`: `base_turn_options` в `LoopConfig`, поддержка сохранения пользовательского усилия между ходами.
- `crates/core/src/subagents.rs`: обновление вызова `LoopConfig`.
- `crates/tui/src/select.rs`: метод `SelectMenu::select_by_value`, unit-тесты.
- `crates/tui/src/lib.rs`: `welcome_card_with_thinking`, обновление подсказок `Tip`.
- `crates/tui/src/main.rs`: перебинд `Alt+O` (все) и `Ctrl+O` (последнее), интерактивное меню `Ctrl+T` (`SelectMenu`), структурированный системный промпт, передача усилия в `spawn_turn`.

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output: 106 passed; 0 failed; 0 ignored; finished in 0.54s.
2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: 0 warnings, 0 errors across all crates.
3. `cargo build -p flashagent-tui`
   Exit code: 0
   Output: Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.25s.

Open questions:
- Нет.

Handoff:
- Запустить TUI (`cargo run -p flashagent-tui -- --model gemma-4-e2b-it-qat@q4_k_xl`) и проверить отображение стадий в свёрнутом виде, переключение по `Ctrl+O` / `Alt+O`, а также меню пресетов по `Ctrl+T`.
