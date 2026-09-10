# 2026-09-07 — B0 этап 2: интерактивный прототип (сборка)

### Status: PASS (код) / PARTIAL (гейт в целом — ждёт вердикта владельца)
### Decision: B0 (прототип рендера, гейт go/no-go)

Files touched:
- crates/ui/Cargo.toml (+wgpu, winit, pollster, bytemuck, swash)
- crates/ui/src/bin/b0.rs (новый, ~720 строк)
- Cargo.toml (workspace: +pollster, +swash)

Verification:
- `cargo clippy -p flashagent-ui --bins -- -D warnings` → Finished, 0 errors/warnings (exit 0)
- `cargo test -p flashagent-ui` → 5 passed; 0 failed (exit 0)
- `cargo build -p flashagent-ui --bins` → Finished (exit 0)
- Запуск невозможен в песочнице (headless, нет Vulkan/GL): визуальная проверка — на машине владельца.

Что внутри b0.rs:
- winit 0.30 окно + wgpu 27 пайплайн (треугольный список, alpha-blend, RGBA-атлас глифов 1024x2048)
- cosmic-text шейпинг + swash растеризация, Mono/Proportional, живой композер
- IME: Preedit (акцентный цвет + подчёркивание) / Commit
- Spring-физика (k=170, c=14) морфа кнопки по клику
- Зацикленные фоновые анимации (blink курсора, дыхание заголовка) параллельно
- Cyrillic hint-строка, Esc/Enter/Backspace, DPR-скейлинг, Fifo vsync
- Известное ограничение: wgpu 27 не даёт документированного into_static —
  использован transmute lifetime surface с SAFETY-обоснованием (Window в App,
  поле объявлено раньше Gpu, дропается позже). В боевом ui-крейте пересмотреть.

Open questions:
- Вердикт гейта B0: FPS, ощущение текста, качество морфа — на машине владельца.

Handoff:
- Владелец: `cargo run -p flashagent-ui --bin b0 --release` (Windows/Linux desktop).
- При go: пометить B0 [x] в ROADMAP.md, стартовать B1 (кит M3 Expressive) и A0 параллельно.

## Addendum (после первого запуска владельца)
- Владелец запустил прототип: окно, кириллица, композер, IME, spring-морф — работают.
- Найдено: цвета выцветшие (sRGB-двойная конверсия). Фикс: выбор non-sRGB формата
  поверхности (значения трактуются буквально). Применён, clippy strict зелёный.
- Ждёт: повторный запуск владельцем + вердикт go/no-go по гейту B0.

## Addendum 2
- Цвета после фикса формата: подтверждено скриншотом (фон чёрный, кнопка M3-фиолетовая).
- Найден баг: пробел не вставлялся (winit шлёт Named(Space), не Character). Исправлен.
