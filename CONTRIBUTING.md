# Contributing to FlashAgent

First off, thank you for considering contributing to FlashAgent! It's people like you who make open-source tools fast, reliable, and delightful to use.

Please take a moment to review this document before submitting contributions.

---

## Philosophy & Core Principles

Before writing code or proposing features, please read **[PHILOSOPHY.md](PHILOSOPHY.md)**.
FlashAgent adheres strictly to a few inviolable principles:
1. **Local-first by default**: No telemetry, no cloud lock-in, no session leakage.
2. **0% Electron / Chromium**: pure native Rust, one binary, about 12 MB of memory when idle.
3. **Rock-solid tool calling**: Tool execution cannot be a lottery. Protocol invariants and self-healing parsing are sacred.
4. **M3 Expressive craft**: High UX fidelity, rich terminal ergonomics, smooth visual feedback.

If a proposed feature contradicts [PHILOSOPHY.md](PHILOSOPHY.md), it will be politely declined.

---

## Development Setup

### Prerequisites
- **Rust 1.85+** (`rustup update stable`)
- Linux, macOS, or Windows
- A local LLM server (e.g. [LM Studio](https://lmstudio.ai), [Ollama](https://ollama.ai), [llama.cpp](https://github.com/ggerganov/llama.cpp), or [vLLM](https://github.com/vllm-project/vllm)) or an OpenAI-compatible API key.

### Building & Testing
```bash
# Clone the repository
git clone https://github.com/flashback7766/FlashAgent.git
cd FlashAgent

# Run the complete test suite
cargo test --workspace

# Run strict clippy linter (warnings are treated as errors)
cargo clippy --workspace -- -D warnings

# Build the release binary
cargo build --release
```

---

## How to Contribute

### 1. Reporting Bugs
- Search existing issues to ensure the bug hasn't already been reported.
- Use the **[Bug Report Template](.github/ISSUE_TEMPLATE/bug_report.md)**.
- Include your OS, terminal, FlashAgent commit/version, LLM backend, and active model.

### 2. Proposing Features
- Open an issue using the **[Feature Request Template](.github/ISSUE_TEMPLATE/feature_request.md)**.
- Discuss the idea with maintainers before embarking on a massive pull request.

### 3. Submitting Pull Requests
1. Fork the repo and create your branch from `main`:
   ```bash
   git checkout -b feature/my-cool-feature
   ```
2. Follow Rust idiomatic conventions:
   - Format with `cargo fmt`.
   - Ensure all tests pass: `cargo test --workspace`.
   - Ensure clippy is happy: `cargo clippy --workspace -- -D warnings`.
3. Keep pull requests focused on a single logical change.
4. Open a Pull Request referencing any related issues.

---

## Code of Conduct
Please note that all participants in this project are expected to adhere to our **[Code of Conduct](CODE_OF_CONDUCT.md)**.

Thank you for helping make FlashAgent better! ⚡
