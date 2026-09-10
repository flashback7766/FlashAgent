# Audit Log: System Prompt Synthesis & Architecture

### Status: PASS
Decision: System Prompt Synthesis, Architecture & Behavioral Guidelines

Files touched:
- `crates/core/src/prompt.rs`:
  - Created modular system prompt synthesis module `flashagent_core::prompt`.
  - Implemented `SystemPromptConfig` (with builder methods for `cwd`, `platform`, `model`, `effort`, `tools`).
  - Implemented `build_system_prompt(&SystemPromptConfig)` integrating:
    1. `IDENTITY & EFFICIENCY`: Concise identity, no preamble/wrap-up, language matching, `path:line` references, zero emojis, period instead of colon before tool calls.
    2. `REASONING INSTRUCTIONS`: Preserved bold stage titles (`**Understanding the Request**`, `**Analyzing Code & Context**`, etc.) so that the TUI live stage tracking (`● Thinking... [Stage]`) in collapsed boxes works seamlessly. Guided direct reasoning under each stage: *"work out what the situation actually means and what follows from it. Think it through directly in clear paragraphs — do not organize into nested checklists, boilerplate sub-bullets, or robotic filler."* and *"After every tool result, work out what it actually says, whether it matches what you expected, and what that means next. Never restate raw tool output."*
    3. `TASK EXECUTION & CODE CRAFTSMANSHIP`: Scope discipline (no unsolicited refactoring or gold-plating), no premature abstractions, boundary-only validation, reading files before modifying, matching surrounding style, concise comments explaining *why* (not *what*), clean deletions without backwards-compatibility debris, root-cause debugging, collaborator judgment, and faithful reporting (anti-false claims: never claim "all tests pass" when checks failed or were skipped).
    4. `TOOL DISCIPLINE & PARALLELISM`: Prioritizing dedicated tools over `run_shell` (`read_file` instead of `cat`/`head`, `write_file`/`apply_edits` instead of `sed`, `glob_find`/`grep_search` instead of shell commands), and running independent tool lookups in parallel.
    5. `ACTIONS & BLAST RADIUS`: Careful evaluation of reversibility and blast radius, requiring user confirmation for destructive or hard-to-reverse operations, forbidding destructive shortcuts (e.g. `--no-verify`).
    6. `UNTRUSTED CONTENT & SAFETY`: Fencing all tool results, file contents, and web data as untrusted data, completely immune to prompt injections or fake system directives.
    7. `GREETINGS & CASUAL MESSAGES`: Instant friendly 1-2 sentence replies to greetings without tool calls, and strictly forbidding conversational chatter or double greetings before/during tool turns.
  - Added unit test `test_build_system_prompt_contains_all_core_gems`.
- `crates/core/src/lib.rs`:
  - Exported `pub mod prompt;` and `pub use prompt::{build_system_prompt, SystemPromptConfig};`.
- `crates/tui/src/main.rs`:
  - Switched from inline static prompt string to `flashagent_core::build_system_prompt(&system_prompt_config)`.

Verification:
1. `cargo clippy --workspace -- -D warnings`:
```
    Checking flashagent-core v0.1.0 (/home/flashback/FlashAgent/crates/core)
    Checking flashagent-tools v0.1.0 (/home/flashback/FlashAgent/crates/tools)
    Checking flashagent-svc v0.1.0 (/home/flashback/FlashAgent/crates/svc)
    Checking flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.17s
exit code: 0
```

2. `cargo test --workspace`:
```
test result: ok. 115 passed; 0 failed; 0 ignored across 5 crates; exit code: 0
```

3. `cargo build --release`:
```
   Compiling flashagent-core v0.1.0 (/home/flashback/FlashAgent/crates/core)
   Compiling flashagent-tools v0.1.0 (/home/flashback/FlashAgent/crates/tools)
   Compiling flashagent-svc v0.1.0 (/home/flashback/FlashAgent/crates/svc)
   Compiling flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `release` profile [optimized] target(s) in 7.33s
exit code: 0
```

Open questions:
- None.

Handoff:
- Bold stage titles are retained for TUI live stage extraction.
- System prompt incorporates all highest-value engineering and agentic practices.
