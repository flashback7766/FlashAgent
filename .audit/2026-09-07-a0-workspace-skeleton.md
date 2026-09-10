# 2026-09-07 — A0: скелет workspace

### Status: PASS
### Decision: A0 (скелет workspace)

Files touched:
- crates/{core,llm,tools,data,proto,svc,tui,app}/Cargo.toml + src (9 крейтов)
- README.md, LICENSE (MIT), .github/workflows/ci.yml
- ROADMAP.md: B0 → [x] (go владельца)

Verification:
- `cargo test --workspace` → ok (5 passed в ui, остальные placeholder, 0 failed)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings (exit 0)

Notes:
- B0 закрыт вердиктом владельца (GO): кириллица, IME, spring-морф, цвета — ок.
- svc зависит от proto+core (каркас границ крейтов), app — точка входа.

Open questions:
- Нет. Следующая веха: A1 (данные) или B1 (кит M3) — по выбору владельца.

Handoff:
- Трек A готов к A1; трек B готов к B1. Рекомендация: A1 первым (ядро без него не живёт), B1 параллельно.
