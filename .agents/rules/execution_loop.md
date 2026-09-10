# Execution Loop

1. Read the target milestone in `ROADMAP.md` + slices of affected code.
2. Minimal change set. Scope = milestone, nothing more.
3. `cargo test --workspace && cargo clippy --workspace -- -D warnings` (+ golden tests for UI).
4. Failure → diagnose directly from compiler output, maximum 2 focused fix attempts per error, then stop and escalate to owner with exact output.
5. Record in `.audit/` using the structure from `AGENTS.md`. Update milestone status in `ROADMAP.md`.
6. Handoff to owner: what was done, how it was verified, what remains open.
