# Audit Log: Resilient Self-Healing Tool Parser, Multi-Question `ask_user`, & Smart Contextual Recaps

### Status: PASS
Decision: Enforce interactive `ask_user` tool usage, self-healing parser for non-standard local model tool calls, multi-question sequential flow, and context-aware turn recaps & answer suggestions

Files touched:
- `crates/llm/src/repair.rs`:
  - Upgraded `repair_json` with a multi-stage self-healing pipeline:
    - Normalizes single-quoted Python dictionaries (`{'question': '...', 'options': [...]}`).
    - Converts Python booleans/None (`True` -> `true`, `False` -> `false`, `None` -> `null`).
    - Strips markdown fences (````json, ````tool_calls) and XML `<tool_call>` tags.
    - Normalizes typographical smart quotes and unescaped control characters (`\n`, `\r`, `\t`) inside JSON strings.
    - Extracts embedded JSON objects/arrays from conversational wrapper text.
- `crates/llm/src/parse.rs`:
  - Enhanced `TextToolScanner` to detect bare tool calls starting with tool indicators (`"name"`, `'name'`, `"tool"`, `'tool'`, `"function"`, `'function'`, `"ask_user"`, `'ask_user'`, etc.).
  - Upgraded `parse_body` to handle:
    - Standard `{"name": ..., "arguments": ...}`.
    - Flat properties without `arguments` wrapper (`{"name": "ask_user", "question": "...", "options": [...]}`).
    - Single-key tool wrappers (`{"ask_user": {"question": "...", "options": [...]}}`).
    - OpenAI function wrappers (`{"function": {"name": ..., "arguments": ...}}`).
- `crates/llm/src/lib.rs`:
  - Re-exported `repair_json` at the root of `flashagent-llm`.
- `crates/tools/src/ask_user.rs`:
  - Added support for sequential multi-question prompts (`questions: [...]`).
  - Added flexible deserialization supporting both string arrays and comma-separated strings (`"Python, Rust, Go"`).
  - Relaxed option bounds (1 to 10 choices) with automatic truncation rather than hard validation failure.
- `crates/tools/src/lib.rs`:
  - Enhanced `parse_args` to auto-repair JSON and unwrap argument envelopes (`arguments`, `parameters`, or single-key wrappers) on deserialization failure.
  - Updated `ask_user` `ToolSpec` schema and description to advertise multi-question and extended choices support.
- `crates/core/src/prompt.rs`:
  - Added strict directive: `CLARIFICATIONS & USER CHOICES (MANDATORY ask_user USAGE)`. Explicitly forbids printing numbered or bulleted question lists as plain text, mandating calling `ask_user` with selectable options.
- `crates/tui/src/main.rs`:
  - Added `extract_choices_from_question_text` and `extract_task_summary`.
  - Upgraded `build_turn_recap_and_suggestion` to detect `ask_user` calls and question turns:
    - Generates purposeful action/purpose recaps (e.g. *"Asked about details to write a simple programming test for junior programmers"*).
    - Extracts sample choices for the composer suggestion (e.g. *"Python, arrays, coding challenge"*), eliminating generic *"What's next?"*.
  - Enriched `generate_recap_and_suggestion` LLM analyzer prompt and filtered out robotic or generic fallbacks for question turns.

Verification:
1. `cargo test --workspace`:
```
running 33 tests in flashagent_llm ... ok (33 passed)
running 43 tests in flashagent_core ... ok (43 passed)
running 33 tests in flashagent_tools ... ok (33 passed)
running 52 tests in flashagent_tui (lib) ... ok (52 passed)
running 3 tests in flashagent_tui (bin) ... ok (3 passed)
running 5 tests in flashagent_ui ... ok (5 passed)
Total passed: 169 passed; 0 failed; 0 ignored; exit code: 0
```

2. `cargo clippy --workspace -- -D warnings`:
```
    Checking flashagent-llm v0.1.0 (/home/flashback/FlashAgent/crates/llm)
    Checking flashagent-core v0.1.0 (/home/flashback/FlashAgent/crates/core)
    Checking flashagent-tools v0.1.0 (/home/flashback/FlashAgent/crates/tools)
    Checking flashagent-svc v0.1.0 (/home/flashback/FlashAgent/crates/svc)
    Checking flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.80s
exit code: 0
```

3. `cargo build --release`:
```
   Compiling flashagent-llm v0.1.0 (/home/flashback/FlashAgent/crates/llm)
   Compiling flashagent-core v0.1.0 (/home/flashback/FlashAgent/crates/core)
   Compiling flashagent-tools v0.1.0 (/home/flashback/FlashAgent/crates/tools)
   Compiling flashagent-svc v0.1.0 (/home/flashback/FlashAgent/crates/svc)
   Compiling flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `release` profile [optimized] target(s) in 8.95s
exit code: 0
```

Open questions:
- None.

Handoff:
- Local models outputting Python-style dicts, flat tool calls, or single-key wrappers are smoothly repaired and executed.
- When clarifying questions or choices are needed, models are strictly instructed to call `ask_user`, and user inputs benefit from concrete multi-choice suggestions in the input composer.
