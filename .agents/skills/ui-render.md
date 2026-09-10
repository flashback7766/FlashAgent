# Skill: ui-render
Trigger: UI text rendering, golden tests, or wgpu layout verification
Inputs: modified crates/ui files
Steps:
1. Run `cargo test -p flashagent-ui`
2. Run `cargo test -p flashagent-ui --features golden`
Verify: cargo test -p flashagent-ui
Forbidden: changing golden frames without owner review
