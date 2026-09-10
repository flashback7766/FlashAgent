# Audit Record: Stable Release Priority and Channel Resolution

### Status: PASS
Decision: M-RELEASE-HARDENING
Files touched:
- `install.sh`
- `install.ps1`

### Summary:
Enforced mandatory stable release priority across POSIX (`install.sh`) and Windows PowerShell (`install.ps1`) installers:
1. **Default Stable Target**: By default, installers first query GitHub's official latest stable release endpoint (`/releases/latest` via GitHub API or web redirect `/releases/tag/<tag>`).
2. **Pre-release Exclusion**: Pre-releases and beta tags (`b*` or releases with `prerelease: true`) are strictly ignored when selecting stable releases.
3. **Beta Phase Graceful Fallback**: If no official stable release exists yet in the repository (i.e. project currently in beta pre-release phase), the installer prints a clear notice and gracefully falls back to the latest pre-release (`b181`).
4. **Automatic Switchover**: Once the first stable release (e.g. `v0.1.0`) is published on GitHub, both installers will automatically and permanently pick that stable release and never install newer `b*` pre-releases by default.
5. **Explicit Channel Overrides**: Added `FLASHAGENT_CHANNEL` / `CHANNEL` (`beta` / `prerelease`) and `FLASHAGENT_VERSION` / `VERSION` for users or CI pipelines that explicitly request pre-releases.

Verification:
- `cargo test --workspace` -> Exit 0 (147 passed)
- `cargo clippy --workspace -- -D warnings` -> Exit 0
- Python Cyrillic scanner -> Exit 0 (0 Cyrillic characters found in codebase)
- `./install.sh` dry test -> Exit 0 (Correctly checks for latest stable release, logs notice of beta phase, and resolves assets)
- `FLASHAGENT_CHANNEL=beta ./install.sh` test -> Exit 0 (Directly resolves beta pre-release assets)

Open questions:
None.

Handoff:
Keep the clean 2-commit history on `main` and tag `b181`. Push amended commit to GitHub remote.
