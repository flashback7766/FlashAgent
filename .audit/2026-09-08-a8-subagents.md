# 2026-09-08 — A8: субагенты (динамические роли, каналы, ревью родителем, наследование прав)

### Status: PASS

### Decision: субагент встраивается как тул `spawn_agent` (выбор владельца: «как тул»);
### логика в `core::subagents` (выбор владельца: core-модуль); каналы — mpsc по id
### (выбор владельца); ревью родителем — результат приходит как tool-результат
### (Role::Tool) + событие в поток. Права наследуются, не расширяются (PHILOSOPHY §6-7).

Files touched:
- crates/core/src/subagents.rs (НОВЫЙ):
  - AgentRole { name, system_prompt, tools } — динамическая роль
  - SubagentSpec { role, prompt, max_steps, timeout, max_output_chars }
  - SubagentMsg { Text, Finished, Cancel } — типизированные mpsc-сообщения по id
  - SubagentHandle { id, rx, task }
  - SubagentHost::new/spawn/live — оркестратор (live-счётчик, mpsc-каналы)
  - SubagentTool — тул spawn_agent (результат = Role::Tool, инъекции не пройдут)
  - SubagentToolFactory — трейт для построения ToolExec ребёнка
  - role_prompt — системные промпты ролей (researcher/coder/reviewer/planner)
  - 5 контрактных тестов (спавн+ответ, таймаут, роль, tool-результат, live-счётчик)
- crates/core/src/permissions.rs:
  - PermissionedTools переведён на владение `Arc<dyn ToolExec>` (раньше заимствовал)
    — нужно, чтобы фабрика возвращала Arc<dyn ToolExec>
  - Добавлен PermissionState::gate() (доступ к гейту для субагента)
- crates/core/src/loop_.rs:
  - ToolExec::as_any() — downcast для тестов/адаптеров
- crates/core/src/lib.rs: pub mod subagents + экспорты
- crates/tools/src/subagents.rs (НОВЫЙ):
  - ToolSubset — ограничение тулов по именам (фильтрует specs, блокирует execute)
  - BuiltinSubagentFactory — PermissionedTools поверх BuiltinTools с общим state
  - CompositeTools + agent_tools — объединение BuiltinTools + spawn_agent
  - 2 теста (subset скрывает/блокирует, пустой subset = всё)
- crates/tools/src/lib.rs: экспорт subagents
- crates/tui/src/main.rs:
  - source → Arc<BackendSource>; spawn_turn берёт Arc
  - perm = PermissionedTools(agent_tools(...)) — родитель видит spawn_agent

Verification:
- cargo test --workspace → 92 passed, 0 failed (core 33: +5 subagents; tools 15: +2 subset)
- cargo clippy --workspace -- -D warnings → 0 warnings
- Смоук headless: cargo build -p flashagent-tui && timeout 3 ./target/debug/flashagent-tui
  → exit 124 (по дизайну, интерактив в headless невозможен) — стартует без паники

Архитектурные решения (зафиксированы владельцем):
- Субагент = тул. Родительский цикл вызывает spawn_agent, внутри отдельный AgentLoop
  с ролью и подмножеством тулов, результат возвращается как tool-результат.
- Каналы: у каждого субагента свой mpsc-приёмник; адресация по id, сообщения типизированы.
- Наследование прав: субагент использует тот же PermissionState (общий гейт) +
  ToolSubset (ограничение тулов) — НИКОГДА не расширяет права.

Известные ограничения (не баги, на будущее):
- Жёсткого лимита параллелизма (максимум живых субагентов) пока нет — есть live-счётчик
  для UI. Cap можно добавить в SubagentHost::spawn (e.g. константа MAX_CONCURRENT).
- Ревью родителем — только через текст результата; нет структурированного accept/reject
  (пока результат-строка достаточно). Событие SubagentFinished в UI-поток не заведено
  (результат виден как tool-результат); при желании — отдельный LoopEvent.

Open questions: живой прогон владельцем — родитель должен увидеть `spawn_agent` в
наборе тулов и вызвать его; проверить, что подтверждение на спавн (Category::Mcp)
срабатывает в Manual-режиме.

Handoff: далее A9 — MCP (клиент, менеджер, маркетплейс-реестр). Перед A9 можно
добавить жёсткий cap параллелизма и структурированный accept/reject для субагентов.
