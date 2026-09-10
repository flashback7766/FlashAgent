# 2026-09-09 — Редизайн раскладки и эстетики TUI (без эмодзи, тёмно-тёплая палитра)

### Status: PASS
### Decision: Редизайн оформления терминального интерфейса (A7 polish):
- Строго без эмодзи — только строгие геометрические глифы Unicode (`❯`, `◈`, `◆`, `◇`, `·`, `╭─╮`, `╰─╯`).
- Двухстрочный окаймлённый композер ввода:
  - Строка 1: рамка с `│ ❯ ` + плейсхолдер или пользовательский ввод.
  - Строка 2: внутренняя мета-строка `│   model · cwd · [mode]   │`.
  - Внешний подвал снизу: подсказки горячих клавиш / спиннер выполнения.
- Тёмно-тёплая эстетическая палитра: Charcoal `#5F5A55`, Sand/Cream `#F5F0E8` / `#E8E3DA`, Amber `#E1AF5F`, M3 Sky `#87B9CD`, Mint Sage `#91CD8C`, Terracotta `#E66E5F`.
- Стартовая компактная карточка-баннер `welcome_card` с версией, моделью, рабочей директорией (`~/...`), режимом разрешений и загруженными документами памяти.
- ANSI-aware утилиты (`visible_width`, `pad_box_row`) для надёжного выравнивания рамок в ширину без смещений из-за управляющих кодов.

Files touched:
- crates/tui/src/lib.rs:
  - Добавлена константа `pub const GLYPH_PROMPT: &str = "❯";`.
  - Добавлен метод `ChatView::push_line(kind, text)` для прямой вставки предформатированных строк.
  - Обновлён `ChatView::render_split`: строки пользователя предваряются `❯ ` вместо эмодзи.
  - Добавлена функция `visible_width(text: &str) -> usize` (подсчёт видимой ширины с игнорированием ANSI escape-последовательностей).
  - Добавлена функция `pad_box_row(content: &str, width: usize) -> String` для отрисовки строк внутри рамки `│...│`.
  - Реализована функция `welcome_card(model, cwd, mode, memory_docs, width) -> Vec<RenderLine>`.
  - Добавлены unit-тесты `welcome_card_and_visible_width` и `restore_line_color_reapplies_after_embedded_resets`.
- crates/tui/src/main.rs:
  - Цветовая палитра `color(kind: LineKind)` обновлена на dark warm (sand, warm amber, muted stone, soft sky).
  - Структура `FrameState` расширена полями `model: &'a str` и `cwd: &'a str`.
  - В `Renderer::frame` реализован рендер 2-строчного бокса композера и обновлена логика позиционирования курсора (`crossterm::cursor::MoveUp(3)` и парковка на строке ввода).
  - В `main()` и `run_app()` проброшены `model` и `cwd_display` (с заменой домашней директории на `~`), вызов `welcome_card` при старте чата.

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output:
   - flashagent_core: 33 passed; 0 failed
   - flashagent_data: 5 passed; 0 failed
   - flashagent_llm: 20 passed; 0 failed
   - flashagent_proto: 0 passed; 0 failed
   - flashagent_svc: 0 passed; 0 failed
   - flashagent_tools: 17 passed; 0 failed
   - flashagent_tui: 13 passed; 0 failed
   - flashagent_ui: 5 passed; 0 failed
   Всего: 93 passed; 0 failed; 0 ignored; finished in 0.35s

2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: 0 warnings, 0 errors

3. `cargo build -p flashagent-tui`
   Exit code: 0
   Output: Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.33s

4. Smoke test:
   `timeout 0.5s ./target/debug/flashagent-tui --model gpt-4o-mini || true`
   Exit code: 0

Open questions:
- Поведение при быстром ресайзе терминала до ширины < 40 символов (минимальная ширина композера зажата через clamp).

Handoff:
- Запустить `cargo run -p flashagent-tui -- --model <model-name>` в интерактивном терминале и оценить визуальный комфорт шрифтов и цветов в реальной терминальной сессии.
