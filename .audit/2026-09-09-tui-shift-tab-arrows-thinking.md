# 2026-09-09 — TUI: переключение режимов по Shift+Tab, стрелочки и разграничение Ctrl+O / Ctrl+Shift+O

### Status: PASS
### Decision: Реализация продвинутого управления в TUI:
1. **Переключение режимов по Shift+Tab**:
   - `PermissionMode` дополнен методами `next()` и `prev()` для циклического обхода: `Manual -> Autonomic -> Planning -> Bypass -> Manual`.
   - В TUI по нажатию `Shift+Tab` (и `KeyCode::BackTab`, и `Tab` с модификатором `SHIFT`) режим циклически переключается с немедленным обновлением отображения в композере.
2. **Управление стрелочками и удаление `a/d - approve`**:
   - Полностью убраны подсказки и обработчики клавиш `a` и `d`.
   - Подтверждение тулов (approval card) переведено на интерактивный селектор `ConfirmSelect`:
     - Клавиши `←` / `→` (а также `↑` / `↓` / `Tab`) переключают выбор между `[Allow]` (мятно-зелёный подсвет) и `[Deny]` (терракотовый подсвет).
     - Клавиша `Enter` подтверждает выбранное решение.
     - Ввод символов в поле ввода заблокирован на время ожидания решения карточки.
   - История ввода в промпте: клавиши `↑` и `↓` в пустом/редактируемом поле листают историю ранее отправленных запросов.
   - Создан модуль `select.rs` (`SelectMenu<T>`, `SelectItem<T>`, `ConfirmSelect`) — универсальная основа для интерактивных меню, будущих настроек, выбора моделей и интерактивных вопросов.
3. **Разграничение `Ctrl+O` и `Ctrl+Shift+O` для рассуждений**:
   - `Ctrl+O` (временный): разворачивает/сворачивает блок мыслей **только для последнего сообщения** (текущего генерируемого или завершенного). Сохраняет состояние до отправки следующего запроса (при нажатии Enter для нового запроса сбрасывается в свернутое состояние по умолчанию).
   - `Ctrl+Shift+O` (постоянный): глобально разворачивает/сворачивает **все блоки мыслей по всей истории**. Сохраняется при отправке новых запросов и отключается только повторным нажатием `Ctrl+Shift+O`.

Files touched:
- crates/core/src/permissions.rs:
  - Реализованы методы `PermissionMode::next()` и `PermissionMode::prev()`.
  - Добавлен unit-тест `permission_mode_cycle_next_prev`.
- crates/tui/src/select.rs (new):
  - Модуль интерактивных селекторов: `ConfirmSelect`, `SelectItem<T>`, `SelectMenu<T>`.
  - Unit-тесты `confirm_select_navigation` и `select_menu_navigation_and_render`.
- crates/tui/src/lib.rs:
  - Экспорт модуля `select`.
  - Структура `ReasoningExpansion { all: bool, last: bool }` с поддержкой `impl Into<ReasoningExpansion>`.
  - Обновлён `render_split`: поддержка раскрытия только последнего блока либо всех блоков одновременно.
  - Обновлён `approval_card`: удалена привязка к `a/d`, добавлен рендер селектора `approval_card_with_selection`.
  - Обновлён `welcome_card`: строка подсказки обновлена на `Ctrl+O toggles thinking · Ctrl+Shift+O toggles all · Esc interrupts`.
  - Добавлен unit-тест `reasoning_expansion_modes_last_and_all`.
- crates/tui/src/main.rs:
  - `FrameState` и `Renderer` переведены на `ReasoningExpansion` и `confirm_selection`.
  - В цикле событий `run_app` реализованы:
    - `Shift+Tab` / `BackTab` -> смена режима.
    - `Ctrl+Shift+O` -> постоянное переключение всех блоков мыслей.
    - `Ctrl+O` -> временное переключение последнего блока мыслей.
    - `Left` / `Right` / `Tab` -> выбор решения в approval card.
    - `Up` / `Down` -> выбор решения в approval card, либо навигация по истории команд в поле ввода.
    - `Enter` -> подтверждение выбранного решения либо отправка промпта со сбросом временного `last_expanded`.

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output: 97 passed; 0 failed; 0 ignored; finished in 0.52s.
2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: 0 warnings, 0 errors.
3. `cargo build -p flashagent-tui`
   Exit code: 0
   Output: Finished `dev` profile.

Open questions:
- Нет.

Handoff:
- Проверить в терминале: переключение режимов по `Shift+Tab`, стрелочки на approval card, `Ctrl+O` и `Ctrl+Shift+O`.
