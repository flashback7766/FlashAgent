# 2026-09-07 — A0: Workspace Skeleton

### Status: PASS
### Decision: A0 (workspace skeleton)

Files touched:
- crates/{core,llm,tools,data,proto,svc,tui,app}/Cargo.toml + src (9 crates)
- README.md, LICENSE (MIT), .github/workflows/ci.yml
- ROADMAP.md: B0 → [x] (owner approval)

Verification:
- `cargo test --workspace` → ok (5 passed in ui, others placeholder, 0 failed)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings (exit 0)

Notes:
- B0 closed by owner verdict (GO): Cyrillic, IME, spring-morph, colors — ok.
- svc depends on proto+core (crate boundaries skeleton), app is entry point.

Open questions:
- None. Next milestone: A1 (data layer) or B1 (M3 kit) — per owner choice.

Handoff:
- Track A ready for A1; Track B ready for B1. Recommendation: A1 first (core depends on it), B1 in parallel.
