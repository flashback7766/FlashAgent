# 2026-09-09 — Фикс локализации подсказки рассуждений и её сохранения после завершения задачи

### Status: PASS
### Decision: Устранение дефектов рендеринга свернутого блока reasoning (TUI polish):
1. **Локализация**: устранена хардкодная русская строка `(ctrl+o — развернуть)` в `crates/tui/src/lib.rs`. Заменена на консистентный английский текст `(ctrl+o to expand)`.
2. **Персистентность подсказки**: ранее суффикс проверял `self.streaming_reasoning == Some(i)`. При завершении генерации мыслей (или по событию `Done`) статус стриминга сбрасывался в `None`, и строка переключалась на ` …`, из-за чего хинт `ctrl+o` исчезал именно тогда, когда пользователь мог захотеть развернуть мысли завершенного хода. Теперь свернутый блок `Reasoning` всегда снабжен `(ctrl+o to expand)`.
3. **Бюджет ширины заголовка стадии**: убран искусственный лимит `width / 2`, заголовок стадии рассуждений использует доступную ширину строки терминала без обрезания.

Files touched:
- crates/tui/src/lib.rs:
  - В `render_split` суффикс строки свернутых мыслей заменён на ` (ctrl+o to expand)` на постоянной основе.
  - Расчёт `budget` скорректирован под ширину экрана.
  - В unit-тест `collapsed_preview_shows_current_stage` добавлена проверка наличия `(ctrl+o to expand)` как во время стриминга, так и после события `LoopEvent::Done`.

Verification:
1. `cargo test --workspace`
   Exit code: 0
   Output: 93 passed; 0 failed; 0 ignored; finished in 0.52s (все тесты crates/tui пройдены успешно).
2. `cargo clippy --workspace -- -D warnings`
   Exit code: 0
   Output: 0 warnings, 0 errors.
3. `cargo build -p flashagent-tui`
   Exit code: 0
   Output: Finished `dev` profile in 1.63s.

Open questions:
- Нет.

Handoff:
- Запустить `cargo run -p flashagent-tui -- --model "gemma-4-e2b-it-qat@q4_k_xl"` и проверить отображение свернутого и развернутого хода мыслей.
