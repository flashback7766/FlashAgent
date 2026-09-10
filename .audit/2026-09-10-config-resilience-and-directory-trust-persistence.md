# Audit: Config Resilience, Test Runner Isolation & Directory Trust Persistence

### Status: PASS
Decision: Fix config wipe during test runs, add backwards-compatible schema deserialization (`#[serde(default)]`), and persist trusted directories across restarts.

Files touched:
- `crates/core/src/config.rs`
- `crates/tui/src/main.rs`

Verification:
- `cargo test --workspace` -> Exit code 0 (197 tests passed, 0 failed).
- `cargo clippy --workspace -- -D warnings` -> Exit code 0 (0 warnings).
- `cargo build --release` -> Exit code 0 (`target/release/flashagent-tui`).

Root Causes Identified & Resolved:
1. **Test Runner Config Overwrite**:
   - `SettingsView` tests executed Enter key events that triggered `self.config.save()`.
   - `AppConfig::default_path()` was defaulting to `$HOME/.flashagent/config.json`.
   - Whenever `cargo test` ran during builds/recompilations, the test runner executed with a default uninitialized config (`setup_completed = false`, `model = ""`) and wiped the user's real `~/.flashagent/config.json`.
   - **Fix**: In `AppConfig::default_path()`, added detection for test runner processes (checking for `/deps/` in `current_exe()` and `FLASHAGENT_CONFIG_PATH`), routing test config writes to a temporary sandbox (`flashagent_test_runner_config.json`) instead of the user's home directory.
2. **Missing Struct-Level `#[serde(default)]`**:
   - `AppConfig` lacked container-level `#[serde(default)]`. Whenever a new field was introduced during development, older `config.json` files failed deserialization and fell back to `Default::default()`, causing `setup_completed` to reset to `false`.
   - **Fix**: Added `#[serde(default)]` to `AppConfig` and added unit test `test_backwards_compatible_deserialization_from_older_schema` confirming older configs preserve `setup_completed = true` and all user choices.
3. **Startup Directory Trust Prompt**:
   - The startup prompt (`Do you trust the contents of this directory?`) previously ran on every launch regardless of prior approval.
   - **Fix**: Added `trusted_directories: Vec<PathBuf>` to `AppConfig`. Once approved, the workspace path is remembered in `config.json` and subsequent launches skip the trust screen automatically.

Open questions: None.
Handoff: Config will never be reset on updates, rebuilds, or test runs. Trusted directories are remembered permanently.
