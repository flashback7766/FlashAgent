# 2026-09-07 — A3: Agent Loop

### Status: PASS
### Decision: A3 (tool loop in crates/core)

Files touched:
- crates/core/Cargo.toml (+flashagent-llm, futures, async-trait, tokio)
- crates/core/src/lib.rs (exports)
- crates/core/src/loop_.rs (AgentLoop + 9 contract tests)

Verification:
- `cargo test --workspace` → TOTAL passed: 39, failed: 0 (core: 9/9)
- `cargo clippy --workspace -- -D warnings` → Finished, 0 warnings

Contract tests (mock LLM, deterministic):
1. plain_text_completes_in_one_turn — pure text, no tools
2. single_tool_call_executes_and_loops — tool → result → final answer
3. multi_tool_calls_execute_in_order — parallel deltas by index, execution ordering
4. stream_interruption_fails_loudly — stream interruption = LoopError, no silent loss
5. step_limit_stops_runaway_loop — runaway model stopped by limit
6. cancel_flag_stops_before_next_turn — Stop button works between steps
7. token_budget_trips_when_usage_exceeds — token budget triggers
8. assistant_reasoning_is_kept_in_history — reasoning preserved in history
9. hostile_tool_result_stays_in_tool_role — injection remains data in Tool role

Design:
- Loop only interacts with LlmSource/ToolExec traits — HTTP/SQL/UI do not exist for it
- All events for UI — LoopEvent (TurnDelta/ReasoningDelta/ToolStarted/ToolFinished/Done)
- DoneReason: Completed/StepLimit/TokenBudget/Cancelled/Failed
- cancel: Arc<AtomicBool>, checked before each step, inside stream, and before each tool

Open questions:
- None.

Handoff:
- Next: A4 — builtin tools (9 tools) + shell isolation, ToolExec implementation.
