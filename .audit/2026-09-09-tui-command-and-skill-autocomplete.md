### Status: PASS
Decision: C5 / TUI Autocomplete (Commands & Skills)
Files touched:
- crates/tui/src/autocomplete.rs
- crates/tui/src/lib.rs
- crates/tui/src/main.rs
- .agents/skills/rust-core.md
- .agents/skills/ui-render.md
- .agents/skills/mcp.md
- .agents/skills/release.md

Verification:
- `cargo test --workspace` -> Exit code 0
  - flashagent_core: 36 passed; 0 failed
  - flashagent_data: 5 passed; 0 failed
  - flashagent_llm: 24 passed; 0 failed
  - flashagent_proto: 0 passed; 0 failed
  - flashagent_svc: 0 passed; 0 failed
  - flashagent_tools: 17 passed; 0 failed
  - flashagent_tui: 27 passed; 0 failed
  - flashagent_ui: 5 passed; 0 failed
  - Total: 114 unit tests passed, 0 failed.
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (0 warnings).

Summary:
- Implemented AutocompletePopup in `crates/tui/src/autocomplete.rs`:
  - Activates when input starts with `/` in the prompt input field.
  - Dynamically renders directly below the input box and above the terminal status footer.
  - Displays built-in commands (`/help`, `/clear`, `/effort`, `/model`, `/expand`, `/mode`, `/goal`, `/mcp`) and contextual subcommands (`/expand all|last|off`, `/effort ...`, `/mode ...`, `/mcp ...`).
  - Scans and dynamically presents custom skills from `.agents/skills/*.md` and `~/.flashagent/skills/*.md` (both `/skill:<name>` and direct `/<name>`).
  - Created 4 base canonical skills per `AGENTS.md`: `rust-core`, `ui-render`, `mcp`, `release`.
  - Supports `Tab` for cyclic completion, `Up`/`Down` arrows to navigate suggestions, and `Esc` to dismiss.
  - Updated renderer cursor math (`lift_up = tail.len() - 1 - input_line_idx` and `prev_cursor_tail_offset`) guaranteeing cursor stays parked on the input prompt row without flickering or displacement.

Open questions:
- None.

Handoff:
- Autocomplete popup for slash commands and skills is active and verified. Ready for next roadmap milestone or user request.
