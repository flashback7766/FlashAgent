<div align="center">

```
  ███████╗██╗      █████╗ ███████╗██╗  ██╗ █████╗  ██████╗ ███████╗███╗   ██╗████████╗
  ██╔════╝██║     ██╔══██╗██╔════╝██║  ██║██╔══██╗██╔════╝ ██╔════╝████╗  ██║╚══██╔══╝
  █████╗  ██║     ███████║███████╗███████║███████║██║  ███╗█████╗  ██╔██╗ ██║   ██║   
  ██╔══╝  ██║     ██╔══██║╚════██║██╔══██║██╔══██║██║   ██║██╔══╝  ██║╚██╗██║   ██║   
  ██║     ███████╗██║  ██║███████║██║  ██║██║  ██║╚██████╔╝███████╗██║ ╚████║   ██║   
  ╚═╝     ╚══════╝╚═╝  ╚═╝╚══════╝╚═╝  ╚═╝╚═╝  ╚═╝ ╚═════╝ ╚══════╝╚═╝  ╚═══╝   ╚═╝   
```

### ⚡ The Local-First, Blazing-Fast AI Coding Agent in 100% Native Rust

[![CI](https://github.com/flashback7766/FlashAgent/actions/workflows/ci.yml/badge.svg)](https://github.com/flashback7766/FlashAgent/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS%20%7C%20Windows-lightgrey.svg)]()
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen.svg)](CONTRIBUTING.md)

**Zero Electron. Zero WebViews. <15ms Cold Start. Deep Local Intelligence.**

[Key Features](#-key-features) •
[Quick Start](#-quick-start) •
[Controls Cheat Sheet](#-controls--shortcuts) •
[Architecture](#-architecture) •
[Contributing](#-contributing)

---

</div>

## 💡 Why FlashAgent?

Most AI coding agents today are packaged inside heavy Electron apps or run exclusively in remote clouds. They eat gigabytes of RAM, take seconds to launch, and send your proprietary code across third-party servers.

**FlashAgent is different:**
- 🦀 **Pure 100% Rust Architecture**: Engineered from scratch for instant responsiveness (<15ms startup time, ~30MB memory footprint, zero Chromium overhead).
- 🔒 **Local-First & Private**: Direct native connection to [LM Studio](https://lmstudio.ai), [Ollama](https://ollama.ai), [vLLM](https://github.com/vllm-project/vllm), or any OpenAI-compatible API endpoint. Your code never leaves your workstation unless you explicitly allow it.
- 🛠️ **Rock-Solid Tool Execution**: Deterministic tool engine with self-healing JSON parsing, strict protocol invariant preservation, and full diff inspection before writing files.
- 🎨 **M3 Expressive Terminal UX**: Live collapsible thinking trees, animated 8-bit mascot, real-time stage progression, and in-place morphing menus that never block the generation stream.

---

## ✨ Key Features

### 🕹️ Mid-Flight Steering (Hybrid Option B)
Don't wait for a 2-minute runaway generation to finish. Type your guidance directly into the composer while the agent is streaming and hit `Enter`:
- **Text & Reasoning Streams**: Instantly soft-interrupts the stream (~200ms turnaround), commits clean partial history, appends your steering directive, and restarts generation without breaking context.
- **Tool Calling Phase**: Respects OpenAI-compatible tool calling protocol invariants (`tool` message must follow `tool_calls`) by allowing the in-flight tool to finish cleanly before applying your steering directive at the exact step boundary.

### ⏱️ Model Prefill Tracker & Exact TTFT Predictor
FlashAgent tracks prompt evaluation speed across context length buckets (`<2k`, `2k-8k`, `8k-32k`, `32k+`) using exponential moving averages:
- Shows real-time visual progress during prompt evaluation: `⚡ Prefill ~1.4s (3.2k tok @ 2.4k t/s) [••••••  ]`.
- Displays exact Time-To-First-Token (TTFT) and processing rates once streaming begins.
- Persists learned profiles locally in `~/.flashagent/prefill_cache.json`.

### 🎯 Autonomous Goal Mode (`/goal <task>`)
Turn FlashAgent from an interactive coding assistant into a fully autonomous engineering sub-agent:
- Automatically elevates permissions, maximizes reasoning effort, runs subagents, self-tests, and verifies changes.
- Automatically rolls back temporary elevated privileges and restored settings once the goal is accomplished.

### 🎛️ Non-Blocking In-Place Menus
Need to tweak sampling, check context, or switch models while the agent is writing code?
- Press `Tab` for Settings, `F3` for Models, `F4` for Thinking Effort, `F5` for Sampling parameters, or `F1` for Context breakdown.
- The composer bottom morphs into the interactive menu while the **chat history and reasoning above continue streaming live tokens uninterrupted**.
- Pressing `Esc` inside any menu closes only the menu without aborting the background generation!

### 🛡️ 4-Tier Granular Permission Engine
Switch modes with `Shift+Tab`:
1. **Manual**: Prompts for confirmation before every modifying command or file write.
2. **Planning**: Enforces plan-first discipline before executing tool actions.
3. **Autonomic**: Streamlined developer mode with automatic approvals for safe operations.
4. **Bypass**: Direct execution (used in Autonomous Goal mode).
*Every file modification presents an interactive visual diff card with `[Allow]` / `[Always]` / `[Deny]` options.*

### 🧰 Production-Grade Toolset
- **Shell Execution**: Interactive or background task execution with timeout protection and input piping.
- **Atomic File Edits**: Multi-file replacement with exact line ranges and rollback safety.
- **Grep & File Discovery**: Fast regex searches and git-aware fuzzy directory walking.
- **Web Browsing & Fetch**: Opt-in web search and markdown documentation extraction.
- **MCP Extensibility**: Connect external tools and sidecars via Model Context Protocol.

---

## ⌨️ Controls & Shortcuts

| Key | Action |
|:---|:---|
| `Enter` | Send message / **Inject Mid-Flight Steering** (during active generation) |
| `Tab` | Open / Close Settings menu (or complete `/` commands) |
| `Shift + Tab` | Cycle permission mode (`Manual` ➔ `Planning` ➔ `Autonomic` ➔ `Bypass`) |
| `F1` | Inspect detailed Context Window breakdown modal |
| `F2` | Cycle reasoning expansion mode (`collapsed` ➔ `last turn expanded` ➔ `all expanded`) |
| `F3` / `Ctrl + M` | Open Model selection menu (with live filter search) |
| `F4` / `Ctrl + T` | Open Thinking Effort preset menu (`off`, `low`, `medium`, `high`, `max`) |
| `F5` | Open Sampling Parameters view (`temperature`, `top_p`, `max_tokens`) |
| `Alt + O` / `Ctrl + O` | Expand / collapse all reasoning blocks |
| `Ctrl + R` | Regenerate last assistant response from scratch |
| `Esc` | Close active menu / Dismiss suggestion / Interrupt active generation |
| `Ctrl + C` | Cancel current generation / Press twice to exit FlashAgent |

---

## 🚀 Quick Start

### 1. Installation

#### From Source (Recommended)
Make sure you have [Rust 1.85+](https://rustup.rs) installed:

```bash
# Clone the repository
git clone https://github.com/flashback7766/FlashAgent.git
cd FlashAgent

# Build the optimized release binary
cargo build --release

# Run FlashAgent directly
./target/release/flashagent-tui
```

#### Pre-built Binaries
Download the latest pre-compiled archive for your OS from [GitHub Releases](https://github.com/flashback7766/FlashAgent/releases).

---

### 2. Connect Your LLM

FlashAgent works out-of-the-box with any OpenAI-compatible server or endpoint:

#### Option A: LM Studio (Local, Zero Setup)
1. Open [LM Studio](https://lmstudio.ai), load your favorite coding model (e.g. `Qwen2.5-Coder-32B`, `DeepSeek-R1-Distill`, `Llama-3.3`).
2. Start the local server on `http://127.0.0.1:1234`.
3. Launch FlashAgent:
   ```bash
   ./flashagent
   ```
   FlashAgent will automatically detect LM Studio, query loaded models, configure thinking capabilities, and start running!

#### Option B: Ollama (Local)
```bash
ollama run qwen2.5-coder:32b
# In FlashAgent, press Tab -> Configure endpoint: http://localhost:11434/v1
```

#### Option C: Cloud Endpoints (OpenRouter, OpenAI, Gemini, DeepSeek)
Set your endpoint and API key via `/settings` (or press `Tab` on launch):
- **Base URL**: `https://api.openai.com/v1` (or `https://openrouter.ai/api/v1`)
- **API Key**: `sk-...`

---

## 🏗️ Architecture

FlashAgent is built around modular, decoupled Rust crates adhering to strict domain boundaries:

```
FlashAgent Workspace
├── crates/core    # Agent loop, state machine, mid-flight steering, permissions, memory
├── crates/llm     # OpenAI-compatible backend engine, streaming parser, thinking protocols
├── crates/tools   # Sandboxed shell, atomic file editors, ripgrep, web fetch, MCP
├── crates/tui     # High-performance crossterm TUI, prefill tracker, M3 expressive tail
├── crates/ui      # Native wgpu renderer, cosmic-text shaping (GPU GUI track)
├── crates/svc     # IPC daemon / service layer
├── crates/data    # SQLite + FTS5 persistent memory and session store
└── crates/proto   # Typed IPC contracts
```

Run test suite across all crates:
```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## 🤝 Contributing

Contributions are warmly welcomed! Whether you are fixing a bug, adding a tool adapter, or improving documentation:

1. Read our **[PHILOSOPHY.md](PHILOSOPHY.md)** to understand our core product principles.
2. Read **[CONTRIBUTING.md](CONTRIBUTING.md)** for our development workflow and code standards.
3. Check **[ROADMAP.md](ROADMAP.md)** to see upcoming milestones.

---

## 📜 License

FlashAgent is released under the **[MIT License](LICENSE)**.

---

<div align="center">

**Crafted with ⚡ and Rust.**

*If you find FlashAgent useful, consider giving it a ⭐ on GitHub to help others discover it!*

</div>
