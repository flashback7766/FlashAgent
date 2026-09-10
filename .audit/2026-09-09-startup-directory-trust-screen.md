### Status: PASS
Decision: Startup Directory Trust & Background Discovery
Files touched:
- crates/tui/src/startup.rs
- crates/tui/src/lib.rs
- crates/tui/src/main.rs

Verification:
- `cargo test --workspace` -> Exit code 0
  - flashagent_core: 36 passed; 0 failed
  - flashagent_data: 5 passed; 0 failed
  - flashagent_llm: 24 passed; 0 failed
  - flashagent_proto: 0 passed; 0 failed
  - flashagent_svc: 0 passed; 0 failed
  - flashagent_tools: 17 passed; 0 failed
  - flashagent_tui: 32 passed; 0 failed (including `test_startup_initial_selection`, `test_startup_navigation_and_numbers`, `test_startup_actions`, `test_startup_change_directory_flow`, `test_startup_rendering_content`)
  - flashagent_ui: 5 passed; 0 failed
  - Total: 119 unit tests passed; 0 failed.
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (Finished dev profile, 0 warnings).
- `cargo run -p flashagent-tui -- --help` -> Exit code 0 (Usage and options printed cleanly).

Summary:
- Implemented `TrustScreen` in `crates/tui/src/startup.rs` matching the exact layout and aesthetics of the directory trust prompt:
  ```text
  > You are in <cwd>

    Do you trust the contents of this directory? Working with untrusted contents comes with higher risk of prompt injection. Trusting the directory allows project-local config, hooks, and exec policies to load.

  > 1. Yes, Continue.
    2. Change working directory.
    3. No, quit.

    Press enter to continue
  ```
- Immediate concurrent background discovery: `backend.discover_server()` is spawned in a background Tokio task right when `main()` starts. While the user reads and navigates the trust screen, LLM server discovery, model detection, context window size, and thinking presets are collected in parallel without any UI delay.
- Option 1 (`1. Yes, Continue.`): confirms trust in the working directory and immediately enters the main TUI loop with already-collected discovery data.
- Option 2 (`2. Change working directory.`): prompts for a new directory path (supports `~`, absolute, and relative paths) with validation (`path.is_dir()`), changes process working directory (`std::env::set_current_dir`), and returns to the trust screen with the new path displayed.
- Option 3 (`3. No, quit.`): cancels discovery and exits cleanly with return code 0 (also triggered via `Esc` or `Ctrl+C`).
- Added CLI options `-y` / `--yes` (and env var `FLASHAGENT_TRUST_DIR`) to bypass confirmation for non-interactive/scripted runs, and `-h` / `--help` for usage display.

Open questions:
- None.

Handoff:
- Startup trust screen and background discovery pipeline are fully functional, thoroughly tested, and verified.
