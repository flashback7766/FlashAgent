# Audit Log: Clean Chat Status Notifications & User-Centric Tips Pool

### Status: PASS
Decision: Eliminate redundant chat scrollback status notices & align tips pool 100% to human user workflows

Files touched:
- `crates/tui/src/main.rs`:
  - Removed redundant `chat.update_or_push_system` calls when `model_changed` or `ctx_changed` fires.
  - Context and model change updates now render exclusively in the composer input placeholder (`› Model context capacity: ...`, `› Server active model switched: ...`) without polluting chat scrollback with bracketed system notices like `[Model context capacity: 192K -> 200K (200k ctx)]`.
  - Removed unused `presets_tag` variable.
- `crates/tui/src/tips.rs`:
  - Completely purged internal plumbing trivia (engine parser details, cosmic-text rendering, IPC, SSE decoding, heuristic recovery, internal UTF-8 boundary checks) and model-directed tool instructions ("Use read_file...", "Use edit_file...", "Prompt FlashAgent to...").
  - Replaced them with 100% human user-centric tips (shortcuts, workflow advice, slash commands, prompt engineering, git/cargo/Linux productivity tips).
  - Maintained pool size at 536 verified, high-value, user-oriented tips.

Verification:
1. `cargo clippy --workspace -- -D warnings`:
```
    Checking flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.97s
exit code: 0
```

2. `cargo test --workspace`:
```
test result: ok. 115 passed; 0 failed; 0 ignored across 5 crates; exit code: 0
```

3. `cargo build --release`:
```
   Compiling flashagent-tui v0.1.0 (/home/flashback/FlashAgent/crates/tui)
    Finished `release` profile [optimized] target(s) in 5.65s
exit code: 0
```

Open questions:
- None.

Handoff:
- Chat history remains pristine: live model and context capacity shifts show only in the input placeholder.
- Tips revolving in the TUI footer are 100% focused on helping the human developer master FlashAgent and terminal workflows.
