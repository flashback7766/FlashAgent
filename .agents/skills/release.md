# Skill: release
Trigger: release checklist, packaging, or audit log handoff
Inputs: clean git tree, passing tests
Steps:
1. Verify all workspace tests pass
2. Verify .audit/ entry is created
3. Verify ROADMAP.md status is updated
Verify: cargo test --workspace && cargo clippy --workspace -- -D warnings
Forbidden: closing milestones without actual verification output
