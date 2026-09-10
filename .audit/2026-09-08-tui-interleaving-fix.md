# Session: TUI delta interleaving fix (A7.1 follow-up)

### Follow-up (same session, owner feedback):
1. **ctrl+o toggle-back**: thinking можно свернуть обратно тем же биндом.
   Renderer.prev_expanded: переворот → reprint_all (\x1b[H\x1b[2J + полная
   перепечатка). Ранее развёрнутый settled-reasoning оставался в скроллбеке.
2. **Windows VT**: enable/disable_virtual_terminal_processing (задел на
   Windows-этап; у владельца kitty — не причина).
3. **Причина отсутствия цветов найдена и исправлена**: две части.
   а) print_line резал строки по char-ширине ВНУТРИ ANSI-последовательностей
   (md() вставляет коды до клиппинга) → ESC отрезался, хвост `[39m` печатался
   литерально. Новый clip_ansi(text, width): CSI копируются целиком, нулевой
   ширины. Тест clip_ansi_keeps_sequences_and_width.
   б) Палитровые цвета (Color::Cyan/Yellow/...) эмитят 38;5;N, а индексы 0-15
   kitty маппит на тему владельца (тёплый крем) — всё сливалось с фоном.
   Подтверждено pty-зондом: коды в потоке были, цвета темы их съедали.
   Фикс: все цвета переведены на truecolor RGB (38;2;r;g;b), тема не влияет.
   Повторный зонд: 109 truecolor, 0 палитровых.

4. **Полусрочное превью reasoning** (запрос владельца): бюджет свёрнутого
   превью = width/2 вместо полной ширины.
5. **Баг «весь текст после форматирования в строке — белый»**: вложенные спаны
   md() (bold/code) закрываются \x1b[0m, что убивает и цвет строки. Фикс:
   restore_line_color(text, prefix) — ре-эмит цвета строки после каждого
   сброса; print_line теперь красит префиксом + restore вместо style(). Тест
   restore_line_color_reapplies_after_embedded_resets. Убран unused Stylize.
6. **Свёрнутый reasoning показывает ТЕКУЩИЙ этап** (запрос владельца):
   reasoning_stage(text) — последний матч шага `N. Title:` / `* Title:` /
   `- Title:` (нумерованный/маркированный; голый `Note:` НЕ этап — фоллбек).
   Превью: `≽ Thinking: <этап>`, авто-смена по ходу стрима. Фоллбек — первая
   строка. Тесты reasoning_stage_tracks_labeled_steps +
   collapsed_preview_shows_current_stage.
7. **Системный промпт с пошаговым форматом** (запрос владельца; gemma-4-e2b
   дрейфует между прогонами): Role::System в llm (as_str "system" + ветка
   сериализации в openai.rs), ChatMessage::system(), TUI кладёт system-
   сообщение с шаблоном Thinking Process (5 шагов) в history[0]. Это же
   гарантирует срабатывание reasoning_stage-детектора этапов.

8. **Раскладка TUI**: welcome-блок с нумерованными tips + cwd + memory, промпт `> `,
   рамка ввода `╭───╮` (верхняя линия), свёрнутое reasoning `↳ Thinking:`.
   Своя глифовая система: ◈ работает / ◆ готово / ◇ ошибка /
   ↳ суб-результаты, braille-спиннер ⠋⠙⠹…. Цвета truecolor,
   свои тексты подсказок. Механики (print-and-forget, мигание, toggle)
   наши с A7.1.

9. **Раскладка как на референс-скрине владельца** (Subscribers View): тул-строка
   как вызов функции — `◈ grep(x) [t1]` в полёте → `◆ grep(x)` + суб-хук
   `↳ ran — N char(s) of output` (ошибка: `◇ …` + `↳ failed`); рамка ввода
   целиком `╭──╮` / `> input` / `╰──╯`; статус при работе — спиннер + режим +
   char(s) + таймер `Ns` + `esc to interrupt`; футер-хинт `⌘ enter — send ·
   ctrl+o — expand thinking`; таймер через turn_started: Instant.
   Аргумент тула — first key:value из args_json (strip {} → split ':' →
   trim кавычек), head 48 символов.

### Status: PASS (основная запись ниже)
### Decision: A7.1 follow-up — fix interleaved reasoning/content deltas (bug from owner screenshot); milestone scope only, no A8 work.
### Files touched:
- `crates/tui/src/lib.rs` — 3 fixes + new test
- `crates/llm/src/openai.rs` — removed unused `use crate::LlmBackend` in tests (warning)

### Changes:
1. **Interleaving fix (main bug)**: `ReasoningDelta` no longer resets `self.streaming`;
   `TurnDelta` no longer resets `self.streaming_reasoning`. Reasoning and content
   stream in parallel: one Reasoning block (merged) + one Assistant line, both
   receive appends regardless of delta order. Answer no longer fragments
   "word per line" when Gemma alternates deltas.
2. **settled_boundary()**: `.or()` chain (first Some) → `.min()` over all live
   indices. First-Some froze an earlier line that still received appends →
   duplicate text in scrollback. min() keeps both live lines repaintable.
3. **Collapsed reasoning (ctrl+o)**: every Reasoning line renders as ONE preview
   row `≽ …` in collapsed mode (live: hint "ctrl+o — развернуть", frozen: `…`).
   Removed `expanded ? usize::MAX` boundary override — unified path, no second
   print of thawed lines when toggling mid-stream (kills scrollback residue).
   `render()` (full) uses expanded=true — tests unaffected.
4. New test `interleaved_reasoning_keeps_assistant_line`: 1 assistant line
   "Привет! Всё хорошо" + 1 merged reasoning block "думаю ещё", streaming=Some(0),
   boundary=0.

### Verification:
- `cargo test --workspace` → 81 passed, 0 failed (was 80; +1 new)
- `cargo clippy --workspace -- -D warnings` → 0 warnings
- Smoke: `timeout 4 cargo run -p flashagent-tui -- --model test --url http://localhost:9999/v1` → starts clean, exit 0
- Note: owner must visually verify live with LM Studio (agent is headless).

### Open questions:
- None blocking. A8 ready to start.

### Handoff:
- Next: A8 subagents (read ROADMAP.md spec; UX mechanics only, no code copying).
- Update ROADMAP.md A7.1 → [x] done. CONTEXT.md status updated.
