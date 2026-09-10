# 2026-09-07 — B0 этап 1: текстовый слой (headless)

### Status: PARTIAL
### Decision: B0 (прототип рендера, гейт go/no-go)

Files touched:
- Cargo.toml (workspace, чистая перезапись после мусора от глитча)
- crates/ui/Cargo.toml (новый)
- crates/ui/src/lib.rs (новый)
- crates/ui/src/text.rs (новый)

Verification:
- `cargo test -p flashagent-ui` → 5 passed; 0 failed (exit 0)
  - latin/cyrillic shaping, метрики, wrap, RTL-bidi — все зелёные
- `cargo clippy -p flashagent-ui -- -D warnings` → Finished, 0 warnings (exit 0)

Open questions:
- Окно + wgpu + IME + spring-морф не проверены: песочница headless, Vulkan-стека нет.
  Визуальная половина B0 выполняется на машине владельца (Windows/Linux desktop).

Handoff:
- Следующий шаг: владелец запускает интерактивную часть прототипа локально,
  либо (рекомендовано) я пишу самодостаточный `cargo run --bin prototype` —
  winit-окно + wgpu + cosmic-text рендер + spring-морф — который владелец
  запустит у себя и оценит. Гейт B0 закрывается его вердиктом.
