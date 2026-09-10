# Audit Log: Dynamic Auto Reasoning Mode & Action-Purpose Recaps

### Status: PASS
Decision: Dynamic Auto Reasoning Mode (`auto`) & Action+Purpose Recap Formatting

Files touched:
- `crates/llm/src/types.rs`:
  - Added `#[default] Auto` to `ThinkingEffort` enum while retaining `Default`, `Off`, `Low`, `Medium`, `High`.
- `crates/llm/src/thinking.rs`:
  - Added `TaskComplexity` enum (`Minimal`, `Low`, `Medium`, `High`).
  - Implemented `analyze_turn_complexity(messages: &[ChatMessage]) -> TaskComplexity` to classify simple greetings, pleasantries, non-code one-liners as `Minimal`, code edits/inspections/compiler errors as `High`.
  - Implemented `ThinkingProfile::resolve_dynamic(&self, messages: &[ChatMessage]) -> Option<&str>` and `ThinkingProfile::resolve_for_complexity(&self, complexity: TaskComplexity) -> Option<&str>` to dynamically pick `off`/`min` for minimal complexity and `on`/`max` for high complexity tasks.
  - Added unit tests `test_analyze_turn_complexity` and `test_resolve_dynamic_binary_and_tiered`.
- `crates/llm/src/openai.rs`:
  - Implemented handling for `ThinkingEffort::Auto` in `request_body` calling `profile.resolve_dynamic(messages)`.
  - Generalized 400 Bad Request error recovery to dynamically capture vendor profile format hints.
  - Updated tests for dynamic effort resolution.
- `crates/llm/src/lib.rs`:
  - Re-exported `TaskComplexity` and `analyze_turn_complexity`.
- `crates/core/src/config.rs`:
  - Changed default `thinking_effort` in `AppConfig::default()` to `"auto"`.
  - Normalized legacy `"default"` / empty strings to `"auto"` during config loading.
- `crates/tui/src/settings.rs`:
  - Replaced `"default"` preset with `"auto"` in `cycle_effort`.
- `crates/tui/src/main.rs`:
  - Updated default `initial_effort` to `"auto"`.
  - Updated effort selection menu to display `"auto"` at the top as `"Auto (dynamically adjusts thinking per turn)"`.
  - Rebuilt recap logic (`build_turn_recap_and_suggestion`):
    - Reads tool calls directly from messages rather than searching text.
    - Formats clear "Action + Purpose" summaries (e.g. "Modified main.rs and verified terminal build to implement the core program", "Greeted the user", "Inspected project files to analyze context and solve the task").
    - Strictly forbids empty robot summaries like "Response generated".
    - Added programmatic safety filter `is_generic_recap` to reject LLM summaries returning "Response generated" / "Response generated" and fall back to clean deterministic summaries.

Verification:
1. `cargo clippy --workspace -- -D warnings`:
```
    Checking flashagent-llm v0.1.0 (/home/flashback/FlashAgent/crates/llm)
    Checking flashagent-core v0.1.0 (/home/flashback/FlashAgent/crates/core)
    Checking flashagent-tools v0.1.0 (/home/flashback/FlashAgent/crates/tools)
    Checking flashagent-svc v0.1.0 (/home/flashback/FlashAgent/crates/svc)
    Checking flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.07s
exit code: 0
```

2. `cargo test --workspace`:
```
test result: ok. 114 passed; 0 failed; 0 ignored across 5 crates; exit code: 0
```

3. `cargo build --release`:
```
   Compiling flashagent-llm v0.1.0 (/home/flashback/FlashAgent/crates/llm)
   Compiling flashagent-core v0.1.0 (/home/flashback/FlashAgent/crates/core)
   Compiling flashagent-tools v0.1.0 (/home/flashback/FlashAgent/crates/tools)
   Compiling flashagent-svc v0.1.0 (/home/flashback/FlashAgent/crates/svc)
   Compiling flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `release` profile [optimized] target(s) in 13.02s
exit code: 0
```

Open questions:
- None.

Handoff:
- `auto` mode is enabled by default across config, settings, and TUI.
- LLM turns dynamically receive reasoning effort (`off` on simple greetings, `on`/`high` on coding/debug).
- Recaps clearly describe what was done and why, never falling back to "Response generated".
