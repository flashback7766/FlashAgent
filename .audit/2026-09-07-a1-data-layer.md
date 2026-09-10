# 2026-09-07 — A1: слой данных

### Status: PASS
### Decision: A1 (SQLite+FTS5)

Files touched:
- Cargo.toml (+rusqlite bundled в workspace)
- crates/data/Cargo.toml (+rusqlite, +tempfile dev)
- crates/data/src/lib.rs (полный: Store, Session, Message, Role, миграции, FTS)

Verification:
- `cargo test -p flashagent-data` → 5 passed; 0 failed (после фикса: schema_version
  хранится TEXT, читался как i64 → InvalidColumnType. Исправлен parse)
- `cargo clippy -p flashagent-data -- -D warnings` → 0 warnings

Open questions:
- Нет.

Handoff:
- Следующая: A2 — LLM-адаптер (OpenAI-совместимый стрим, единый ToolCall, мультипарсер).
