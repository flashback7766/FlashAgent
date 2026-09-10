# Skill: rust-core
Trigger: build, test, or clippy verification of Rust workspace crates
Inputs: modified Rust source files
Steps:
1. Run `cargo test --workspace`
2. Run `cargo clippy --workspace -- -D warnings`
3. Check exit codes and fix any warnings or failures
Verify: cargo test --workspace && cargo clippy --workspace -- -D warnings
Forbidden: ignoring compiler warnings, adding allow(warnings), or skipping tests
