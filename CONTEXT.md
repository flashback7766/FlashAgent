# FlashAgent — Context for AI Agents (compaction-safe)

> Цель этого файла: любой новый агент (или этот же после компактирования контекста)
> читает его и продолжает работу без потери нити. Обновлять при каждом закрытии вехи.

## Что это за проект
Полная перезапись flashgent (Electron, legacy в /home/flashback/flashgent-dev) в
**~/FlashAgent**: локальный агент-усилитель с /goal-автономом, Rust-ядро +
собственный wgpu-рендер (M3 Expressive), без Electron/веба. Открытый продукт (MIT).

## Иерархия истины (читать в этом порядке)
1. PHILOSOPHY.md — канон, ЧТО и ЗАЧЕМ (~110 решений владельца, менять только с его явного указания)
2. ARCHITECTURE.md — КАК: 9 крейтов, границы, протоколы
3. ROADMAP.md — вехи и статусы (A=ядро, B=UI, C=продукт)
4. .agents/rules/ — code_quality, token_discipline, execution_loop
5. .audit/ — журнал сеансов (append-only), одна запись = один сеанс
6. CONTEXT.md (этот файл) — быстрый вход + текущее состояние

## Стек (решено окончательно)
Rust 1.85+, workspace из 9 крейтов: core (цикл), llm (бэкенды+парсеры),
tools (тулы+MCP), data (SQLite+FTS5), proto (IPC), svc (сервис tokio),
ui (wgpu+cosmic-text+M3E), tui (терминал-клиент), app (entry).
LLM: OpenAI-совместимые бэкенды (любые локальные и удалённые endpoints: LM Studio,
Ollama, vLLM, OpenRouter и любые совместимые модели). llama.cpp встроенный — отдельная фаза позже.
Лицензия MIT. Релизы: GitHub Releases, Stable+Beta. Телеметрия opt-in счётчики.

## Статус (обновляй!)
- B0 гейт рендера: ✅ PASS — владелец дал GO (прототип b0.rs: окно, кириллица, IME,
  spring-морф; фикс цветов = non-sRGB 8-bit формат; фикс пробела = Named(Space))
- A0 скелет: ✅ | A1 данные: ✅ 5/5 | A2 LLM: ✅ 20/20 | A3 цикл: ✅ 9/9 | A4 тулы: ✅ 13/13 |
  A5 разрешения: ✅ 14 (10 permissions + 4 diff) | A6 память: ✅ 5 | A7 TUI: ✅ 4 |
  A8 субагенты: ✅ (core::subagents + tools::subagents)
- Воркспейс: 92 passed, 0 failed, clippy --workspace -- -D warnings чисто
- СЛЕДУЮЩАЯ ВЕХА: A9 — MCP (клиент, менеджер, маркетплейс-реестр).
- Баг TUI (чередование reasoning/content-дельт) ИСПРАВЛЕН 2026-09-08:
  reasoning и контент стримятся параллельно (ReasoningDelta не закрывает
  assistant-строку и наоборот); settled_boundary = min() живых индексов;
  свёрнутый reasoning — всегда одна preview-строка (нет остатка в скроллбеке
  при ctrl+o). Тест interleaved_reasoning_keeps_assistant_line. Владельцу:
  визуальный прогон с LM Studio перед A8.
- Доп. фиксы A7.1 по фидбеку владельца (2026-09-08, kitty): truecolor вместо
  палитры (38;2 вместо 38;5 — тему терминала больше не трогает), clip_ansi
  (неразрывные ANSI при клиппинге), restore_line_color (цвет строки
  восстанавливается после вложенного md()-форматирования), ctrl+o toggle-back
  (полный repaint при перевороте), превью reasoning = полширины. 85 тестов.
- Свёрнутый reasoning показывает ТЕКУЩИЙ этап мышления (reasoning_stage:
  последний матч `N. Title:` / `* Title:`), авто-смена по стриму, фоллбек —
  первая строка. Баг «после md()-форматирования строка белеет» исправлен
  (restore_line_color).
- Системный промпт введён: Role::System + ChatMessage::system() в llm,
  TUI кладёт шаблон Thinking Process (5 шагов) в history[0] — стабилизация
  формата размышлений gemma-4-e2b + гарантия работы детектора этапов.
- Раскладка TUI: собственная эстетичная глифовая система
  (авторское отличие): ◈/◆/◇ для тулов (в полёте/ок/ошибка), ↳ для
  суб-строк и reasoning, braille-спиннер ⠋⠙⠹, рамка ввода ╭──╮+
  ╰──╯ с футер-хинтом, статус с таймером Ns · esc to interrupt,
  welcome с нумерованными tips. Тул-строка — как вызов функции:
  `◆ grep(x)` + `↳ ran — N char(s)`. 85 тестов.
- Сделано в A7.1 (print-and-forget рендерер): settled → скроллбек один раз
  (printed_settled), live-хвост перерисовка \x1b[{n}F+\x1b[J, HARD CAP строк по ширине
  (фикс дубляжа), превью reasoning в ширину, md() bold/code, спиннер ·✢✳✶✻✽,
  ✓/✗ тул одной строкой, Esc = interrupt + маркер, ctrl+o = thinking toggle.
- Заметка A7/A7.1: TUI in-process (BackendSource → AgentLoop → PermissionedTools →
  BuiltinTools); svc/IPC — отдельная веха. Рендер — print-and-forget: settled строки в скроллбек один раз, live-хвост перерисовывается; ctrl+o —
  thinking, md() bold/code, спиннер ·✢✳✶✻✽, ✓/✗ тула одной строкой, Esc = interrupt.
  Владелец прогоняет: cargo run -p flashagent-tui -- --model <имя> --url http://localhost:1234/v1
- Архитектурная заметка A5: разрешения — обёртка PermissionedTools поверх ToolExec
  (core::permissions), цикл не знает про них; UI-интеграция — трейт ApprovalGate;
  диффы — WritePreview на executor'е + core::diff::unified.
- Архитектурная заметка A8 (субагенты): core::subagents — AgentRole (имя+промпт+
  тулы-подмножество), SubagentSpec (роль+задача+лимиты), SubagentHost (оркестратор,
  mpsc-каналы по id, live-счётчик), SubagentTool (тул spawn_agent, результат =
  Role::Tool), SubagentToolFactory. tools::subagents — ToolSubset (ограничение
  тулов), BuiltinSubagentFactory (PermissionedTools поверх BuiltinTools с общим
  PermissionState), CompositeTools + agent_tools (объединение BuiltinTools +
  spawn_agent). Наследование прав: субагент использует тот же PermissionState,
  НИКОГДА не расширяет (канон PHILOSOPHY §6-7). Рефакторинг: PermissionedTools
  теперь владеет Arc<dyn ToolExec> (раньше заимствовал), ToolExec::as_any добавлен.
  В TUI source стал Arc<BackendSource>, spawn_turn берёт Arc.

## Верификация (обязательна перед закрытием любой вехи)
```
cargo test --workspace
cargo clippy --workspace -- -D warnings
```
UI дополнительно golden-кадры позже. Никогда не закрывать веху без реального вывода.

## Критичные решения, которые легко сломать
- Границы крейтов: core НЕ знает HTTP/SQL/UI (трейты LlmSource/ToolExec)
- Инъекции: tool-результат всегда Role::Tool, тест hostile_tool_result_stays_in_tool_role
- sRGB: surface формат non-sRGB 8-bit (Bgra8Unorm/Rgba8Unorm), иначе цвета выцветают
- Пробел в winit: Key::Named(NamedKey::Space), не Character
- wgpu 27: into_static нет, transmute с SAFETY (Window в App живёт дольше Gpu)
- FTS5 schema_version хранится TEXT
- Usage парсится до choices; сканер текстовых тулов НЕ в HTTP-потоке (подключается в цикле при необходимости)

## Окружение
- Песочница агента: headless Linux (Vulkan нет), запуск GUI невозможен — только
  компиляция/тесты. Визуальные проверки делает владелец на своей машине.
- Владелец: Windows, PowerShell (разделитель `;`), русский язык. Открыт OpenRouter.
- Известная проблема: обрывы стриминга API — после обрыва продолжать с текущего места,
  сверяясь с .audit/ и ROADMAP.md, не переделывая закрытое.

## Стиль работы с владельцем
- Не задавать несколько БЛОКОВ вопросов за раз (один ask_question = один батч)
- Вехи без дат; скоуп = веха; «ещё заодно» = нарушение
- Отчёты структурой: Status/Decision/Files/Verification/Open questions/Handoff
