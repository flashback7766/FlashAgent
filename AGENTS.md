# AGENTS.md — Rules for AI Agents in FlashAgent

## Sources of Truth (read in this strict order)
1. `PHILOSOPHY.md` — WHAT and WHY. The Canon. Any contradiction = agent error.
2. `ARCHITECTURE.md` — HOW. Crate boundaries, IPC protocols, core traits.
3. `ROADMAP.md` — IN WHAT ORDER. Milestones, progress statuses, B0 gate rule.
4. `.agents/rules/` — Execution discipline.
5. `.audit/` — Audit trail log. Append-only, never edit historical entries.

## Agent Boundaries
- The agent NEVER modifies PHILOSOPHY.md without explicit instruction from the project owner in chat.
- The agent NEVER expands milestone scope. "While we are at it" = immediate violation.
- Architectural decisions with 2+ valid paths → stop, outline options + recommendation + single-sentence rationale, await owner decision.
- Every milestone ends with verification using real command output, never narrative paraphrase.

## Skills (Overhauled System)
A skill is a dedicated file at `.agents/skills/<name>.md` with a strict structure:
```markdown
# Skill: <name>
Trigger: <when applied>
Inputs: <required inputs>
Steps:
1. ...
Verify: <command + expected output>
Forbidden: <what the skill never does>
```
A skill is activated when referenced by name in a task. Without Trigger/Verify, a skill is invalid and will not be used.
Base set is created progressively alongside milestones (`rust-core`, `ui-render`, `mcp`, `release`).

## Project Audit (Overhauled System)
- Log format: `.audit/YYYY-MM-DD-<slug>.md`, one record per working session.
- Record structure: `### Status: PASS|FAIL|PARTIAL`, `Decision:` (milestone ID), `Files touched:`, `Verification:` (exact commands + exit codes), `Open questions:`, `Handoff:` (what the next agent must do).
- The audit record is created BEFORE completing the response turn, never deferred.
- Discrepancy between `.audit` and `ROADMAP.md`: `ROADMAP.md` wins; audit is historical evidence.

## Verification
- Core engine: `cargo test --workspace && cargo clippy --workspace -- -D warnings`
- UI: the same (`cargo test -p flashagent-ui`); golden frames are planned for B1 — no `golden` feature exists yet
- No milestone is closed without the actual command execution output present in the audit log.
