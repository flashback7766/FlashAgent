# Contributing to FlashAgent

Read [PHILOSOPHY.md](PHILOSOPHY.md) first. A feature that goes against it will be declined, however well it is built. In short:

1. **Local first.** No telemetry, no cloud lock-in; a session goes only to the provider you chose.
2. **No Electron or Chromium.** One native Rust binary, about 10 MB of memory when idle ([measured](docs/numbers.md)).
3. **Tool calling that works every time.** A change that makes calls less reliable is not accepted.
4. **A finished interface.** Details matter in the terminal as much as in a GUI.

## Setup

You need Rust 1.88 or newer and Linux, macOS or Windows. For trying changes by hand, a model server ([LM Studio](https://lmstudio.ai), [Ollama](https://ollama.com), [llama.cpp](https://github.com/ggml-org/llama.cpp), [vLLM](https://github.com/vllm-project/vllm)) or a cloud API key. The tests need neither: they run against a scripted mock server.

```bash
git clone https://github.com/flashback7766/FlashAgent.git
cd FlashAgent
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --release
```

CI runs clippy from the latest stable Rust, which can have lints an older toolchain lacks, and checks that the workspace builds on 1.88.

## Bugs and ideas

- **Bug:** search the issues first, then use the [bug report template](.github/ISSUE_TEMPLATE/bug_report.md). Give your OS, terminal, FlashAgent version, server and model.
- **Feature:** open a [feature request](.github/ISSUE_TEMPLATE/feature_request.md) and agree on it before writing a large pull request.

## Pull requests

1. Branch from `main`.
2. Write like the code around you. **Do not run `cargo fmt`**: the code is not rustfmt-formatted, and it would rewrite every file. Comments say why, not what.
3. `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` must pass. A bug fix comes with a test that fails without it.
4. One logical change per pull request, linked to its issue.

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md).
