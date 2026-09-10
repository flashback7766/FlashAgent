# Audit Record: Installer Edge-Case Resilience and Multi-Platform Hardening

### Status: PASS
Decision: M-RELEASE-HARDENING
Files touched:
- `install.sh`
- `install.ps1`

### Summary:
Addressed all cross-platform installation edge cases across POSIX (Linux/macOS) and Windows PowerShell environments:
1. **GitHub API Rate-Limit Immunity**: Implemented fallback web scraping via GitHub releases and `/expanded_assets/<tag>` endpoints, ensuring reliable installations in environments where unauthenticated GitHub API 60 req/hr limits are exhausted (shared IPs, CI, VPNs).
2. **Version Pinning Support**: Added `FLASHAGENT_VERSION` and `VERSION` environment variable handling in `install.sh` and `-Version` / `$env:FLASHAGENT_VERSION` in `install.ps1`, allowing installation of specific releases and beta tags.
3. **Directory Customization**: Supported user overrides via `INSTALL_DIR` and `BIN_DIR` before falling back to system-wide `/usr/local/bin` (root) or `~/.local/bin` (non-root) on Linux/macOS and `%LOCALAPPDATA%\Programs\FlashAgent` on Windows.
4. **macOS Apple Silicon & Intel Regex Fix**: Generalized release asset pattern regexes (`macos.*aarch64.*\.tar\.gz` and `macos.*x86_64.*\.tar\.gz`) to match versioned release names (`flashagent-macos-b181-aarch64.tar.gz`).
5. **macOS Gatekeeper Quarantine Removal**: Automatically executes `xattr -dr com.apple.quarantine` on macOS to eliminate gatekeeper warnings.
6. **Alpine Linux musl Compatibility**: Automatically provisions `gcompat` when `/etc/alpine-release` is detected so glibc-linked binaries run without missing interpreter errors.
7. **Recursive Binary Search**: Searches archive contents by executable name (`flashagent` or `flashagent-tui`) before relying on executable bit flags, ensuring unpack success even if tar archives strip file modes.
8. **In-Flight Process Replacement**: Retained atomic replacement routines (`flashagent.old`) on both POSIX (`ETXTBSY` avoidance) and Windows (`Safe-InstallExe` locked file handling).
9. **Shell PATH Propagation**: Universal variable and configuration registration for Fish (`fish_add_path -U`), Bash (`.bashrc` and `.bash_profile`), Zsh (`.zshrc` and `.zprofile`), POSIX (`.profile`), and Windows Registry (`HKCU\Environment\Path` + `$env:Path`).

Verification:
- `cargo test --workspace` -> Exit 0 (147 passed)
- `cargo clippy --workspace -- -D warnings` -> Exit 0
- Python Cyrillic scanner -> Exit 0 (0 Cyrillic characters found in codebase)
- `./install.sh` local execution -> Exit 0 (Successfully installed to `~/.local/bin/flashagent`)
- `bash -l -c "flashagent --version"` -> Exit 0 (`FlashAgent b181`)
- `fish -c "flashagent --version"` -> Exit 0 (`FlashAgent b181`)

Open questions:
None.

Handoff:
Keep the clean 2-commit history on `main` and tag `b181`. Push amended commit to GitHub remote.
