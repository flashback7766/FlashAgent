# Audit Log: Greeting Efficiency & Tool Discipline System Prompt

### Status: PASS
Decision: Prompt & Agent Behavioral Polish (Greeting efficiency & eliminating premature tool chat / dual greetings)

Files touched:
- `crates/tui/src/main.rs`: Refined main assistant system prompt:
  - Added explicit `GREETINGS & CASUAL MESSAGES` rule: for greetings/acknowledgements without a task, respond immediately in 1-2 friendly sentences without tool calls or memory scans.
  - Added explicit `NO CONVERSATIONAL CHATTER BEFORE OR DURING TOOL CALLS` rule: when invoking tools, forbid premature greetings ("Привет! Чем могу помочь?", "Готово") in tool-calling turns, and strictly forbid greeting the user twice across steps.
  - Adaptive reasoning breakdown: clarified that stages are selected per task relevance (1-2 brief stages for greetings/clarifications instead of forcing rule inspection).

Verification:
- `cargo test --workspace`:
```
test result: ok. 112 passed; 0 failed; 0 ignored across 5 crates; exit code 0
```
- `cargo clippy --workspace -- -D warnings`:
```
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.62s
exit code 0
```
- `cargo build --release`:
```
Finished `release` profile [optimized] target(s) in 9.11s
exit code 0
```

Open questions:
- None.

Handoff:
- FlashAgent now handles simple greetings like "Привет!" cleanly, answering within 1 turn without spinning up file-search tools or stuttering greetings.
