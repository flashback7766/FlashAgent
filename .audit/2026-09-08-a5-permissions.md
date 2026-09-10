# 2026-09-08 — A5: разрешения

### Status: PASS
### Decision: A5 закрыт. Слой разрешений — обёртка ToolExec (PermissionedTools) в core,
### цикл не изменён (9 тестов A3 нетронуты). По канону PHILOSOPHY §6 / ARCHITECTURE §5.

Files touched:
- crates/core/src/diff.rs (НОВЫЙ: unified diff, LCS, trim prefix/suffix, cap 1500 строк → summary)
- crates/core/src/permissions.rs (НОВЫЙ: PermissionMode 4 шт, Category, Verdict, RuleSet,
  parse_chain с кавычками, PermissionState, ApprovalGate-трейт, DenyAllGate/AllowAllGate,
  PermissionedTools)
- crates/core/src/loop_.rs (трейт WritePreview — опциональная способность executor'а)
- crates/core/src/lib.rs (экспорты)
- crates/tools/src/fs_tools.rs (apply_edits — чистая функция, read_raw)
- crates/tools/src/lib.rs (BuiltinTools: WritePreview — диффы write/edit до записи)

Verification:
- cargo test --workspace → 68 passed, 0 failed
  (core 23 = loop 9 + diff 4 + permissions 10; tools 15; llm 20; data 5; ui 5)
- cargo clippy --workspace -- -D warnings → Finished, 0 warnings

Ключевые свойства (по канону):
- 4 режима: Manual (карточка на каждый write/shell/mcp), Autonomic (всё run),
  Planning (read+net only, остальное Deny), Bypass (всё run без вопросов)
- Узкие правила: parse_chain рвёт по &&/;/|/\n вне кавычек; каждая секция должна
  совпасть с префиксом → "npm test && npm publish" НЕ проходит, "npm testcase" НЕ проходит
- Matрица категорий: Read/Net всегда; Write — diff-превью обязательно в Manual;
  Shell — префиксные правила; Mcp (неизвестные тула) — превью аргументов
- Сессионные правила: allow_tool_always / allow_shell_prefix / deny_tool (blacklist
  бьёт всё, включая Bypass)
- Diff-конвейер: BuiltinTools реализует WritePreview — unified diff считается ДО
  записи (apply_edits in-memory), отдаётся в карточку ApprovalRequest.diff
- Интеграция UI: ApprovalGate-трейт (async approve) — UI рисует карточку Allow/Deny

Open questions: нет (матрица-опция покрыта категориями; split-дифф — задача UI-трека)

Handoff: следующая веха — A6 (память: MEMORY.md проект+глобал, подхват чужих форматов,
пороговая инъекция). Читать ROADMAP.md.
