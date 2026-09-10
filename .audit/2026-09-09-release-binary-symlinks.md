# Audit: Release Binary Compilation and Root Symlinks

### Status: PASS
Decision:
Compile `flashagent-tui` in `--release` profile and create symlinks in the workspace root (`./flashagent` and `./flashagent-tui` pointing to `target/release/flashagent-tui`) so that the user can run the executable directly via `./flashagent` or `./flashagent-tui`, and any subsequent release builds will automatically update the binary.

Files touched:
- `flashagent` (symlink -> `target/release/flashagent-tui`)
- `flashagent-tui` (symlink -> `target/release/flashagent-tui`)

Verification:
1. `cargo build --release -p flashagent-tui`
   Exit code: 0
   Built `target/release/flashagent-tui` (9.1MB).

2. `./flashagent --help`
   Exit code: 0
   Prints help usage successfully.

3. Zero emojis:
   0 emojis found.

Open questions:
- None.

Handoff:
- The user can now run `./flashagent` or `./flashagent-tui` directly from the repository root.
