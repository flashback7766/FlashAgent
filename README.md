<div align="center">

<a href="https://github.com/flashback7766/FlashAgent">
  <img src="assets/banner.svg" alt="FlashAgent — The Local-First AI Coding Agent" width="880" />
</a>

<br/>

### Fast, local-first autonomous AI coding agent in 100% native Rust

[![CI](https://github.com/flashback7766/FlashAgent/actions/workflows/ci.yml/badge.svg)](https://github.com/flashback7766/FlashAgent/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/flashback7766/FlashAgent?include_prereleases&color=cba6f7&label=version)](https://github.com/flashback7766/FlashAgent/releases)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-89b4fa.svg)](https://github.com/flashback7766/FlashAgent/releases)
[![Discussions](https://img.shields.io/badge/discussions-join-a6e3a1.svg)](https://github.com/flashback7766/FlashAgent/discussions)

**&lt; 7ms cold start · &lt; 10MB memory footprint · Zero Electron · Zero cloud lock-in**

<br/>

<img src="docs/screenshots/gifs/hero-chat.gif" width="100%" alt="FlashAgent terminal UI in action">

</div>

By [flashback7766](https://github.com/flashback7766).

> **Status:** Beta b218 (Active). See [ROADMAP.md](ROADMAP.md) and [PHILOSOPHY.md](PHILOSOPHY.md).

---

## ⚡ Why FlashAgent?

Most AI coding agents today are packaged inside heavy Electron apps or run exclusively against remote cloud APIs. They consume gigabytes of RAM, take seconds to launch, stream proprietary code over third-party servers, and offer sluggish terminal experiences.

**FlashAgent is engineered from scratch in 100% native Rust** for instant launch, uncompromising privacy, and seamless local model execution.

| Feature / Metric | **FlashAgent** ⚡ | **Electron Agents** (Cursor, Windsurf) | **Python CLI Agents** (Aider, OpenDevin) |
| :--- | :---: | :---: | :---: |
| **Startup Latency** | **&lt; 7 ms** *(measured ~5.9 ms)* | 4 – 8 seconds | 1.5 – 3 seconds |
| **Memory Footprint (Idle)** | **&lt; 10 MB RSS** *(measured 9.7 MB)* | 1.2 – 2.5 GB (Chromium) | 350 – 800 MB (Python runtime) |
| **Distribution** | **15 MB Native Binary** *(~6.1 MB compressed .tar.gz)* | 150 – 300 MB installer | Requires Python 3.10+ & `pip` venvs |
| **Runtime Dependencies** | **None** *(Pure Native Binary, 0 dynamic interpreters)* | Node.js + Chromium + WebViews | Python interpreter + wheel packages |
| **Local LLM Support** | **First-Class Native** *(LM Studio, Ollama, llama.cpp, vLLM)* | Cloud-first (Local is secondary/limited) | Generic HTTP via litellm adapters |
| **Mid-Flight Steering** | **Instant soft-interrupt (&lt; 50ms)** | Abort & lose full turn | Interrupt resets conversation turn |
| **Data Privacy** | **100% Local / Zero Telemetry** | Continuous cloud telemetry | Depends on backend configuration |
| **In-App Updater** | **Atomic binary swap (`Stable` / `Beta`)** | Heavy background update daemons | Manual `pip install --upgrade` |

---

## 🕹️ Mid-Flight Steering

Don't wait for a 2-minute runaway generation to finish. Type your guidance directly into the composer while the agent is streaming tokens and press <kbd>Enter</kbd>.

<div align="center">
<img src="docs/screenshots/gifs/steering.gif" width="100%" alt="Mid-flight steering in FlashAgent">
<br><sub><b>Mid-flight steering</b> — soft-interrupts active generation, preserves reasoning history, and injects guidance without breaking tool protocol invariants.</sub>
</div>

### How It Works
- **Text & Reasoning Streams**: Instantly soft-interrupts the stream (< 50ms turnaround), commits clean partial history and completed reasoning stages into conversation history, appends your steering directive, and restarts generation without breaking context.
- **Tool Calling Phase**: Respects OpenAI-compatible tool calling protocol invariants (`tool` message must follow `tool_calls`) by allowing any in-flight atomic disk write to finish cleanly before applying your steering directive at the exact step boundary.

---

## 🎛️ Non-Blocking Menus & Tool Execution

Need to switch models, check context tokens, or inspect file diffs while code is actively generating? The TUI stays responsive at all times.

<table>
<tr>
<td width="50%"><img src="docs/screenshots/gifs/menus.gif" alt="In-place non-blocking menus"><br>
<sub><b>In-place menus</b> — <kbd>F3</kbd> model picker with live fuzzy search across LM Studio, <kbd>F4</kbd> thinking effort presets, <kbd>F5</kbd> sampling controls, and <kbd>Tab</kbd> settings.</sub></td>
<td width="50%"><img src="docs/screenshots/gifs/tools-diff.gif" alt="Tool execution and inspection"><br>
<sub><b>Tool execution & inspection</b> — sandboxed shell execution, atomic file reads, and interactive diff inspections before disk writes.</sub></td>
</tr>
</table>

- **Morphing Composer**: Pressing <kbd>F3</kbd>, <kbd>F4</kbd>, <kbd>F5</kbd>, or <kbd>Tab</kbd> morphs the bottom composer into an interactive selection menu while the **chat history and reasoning above continue streaming live tokens uninterrupted**.
- **Safe Dismissal**: Pressing <kbd>Esc</kbd> closes only the active menu without aborting the background generation.

---

## ⏱️ Predictive Prefill & TTFT Speedometer

FlashAgent tracks prompt evaluation speed across context length buckets (`<2k`, `2k-8k`, `8k-32k`, `32k+`) using exponential moving averages:
- Shows real-time countdowns during prompt evaluation: `⚡ Prefill ~1.4s (3.2k tok @ 2.4k t/s) [••••••  ]`.
- Displays exact Time-To-First-Token (TTFT), token generation speed (`tg: 84.2 t/s`), and MTP draft acceptance rates once streaming begins.
- Profiles persist locally across runs in `~/.flashagent/prefill_cache.json`.

---

## 🎯 Autonomous Goal Mode (`/goal <task>`)

Turn FlashAgent from an interactive copilot into a self-directed software engineer:
- Automatically elevates permissions, maximizes reasoning effort, runs sub-agents, self-tests, and verifies changes.
- Executes commands, creates files, runs builds, inspects compiler diagnostics, and iterates autonomously until all goals pass.
- Automatically rolls back temporary elevated privileges and restored settings once the goal is accomplished.

---

## 🛡️ 4-Tier Granular Permission Engine

Switch permission tiers on the fly with <kbd>Shift</kbd> + <kbd>Tab</kbd>:
1. **Manual**: Prompts for confirmation before every modifying command or file write.
2. **Planning**: Enforces plan-first discipline before executing tool actions.
3. **Autonomic**: Streamlined developer workflow with automatic approvals for safe reads and builds.
4. **Bypass**: Direct execution for unattended autonomous batch runs.

*Every file modification presents an interactive visual diff card with `[Allow]` / `[Always]` / `[Deny]` options.*

---

## 🔄 In-App Self-Updater & Channel Switching

Keep your installation fresh without visiting GitHub releases or piping curl scripts:
```bash
flashagent --update                  # Update on current channel
flashagent --channel beta --update   # Switch to Beta and update
flashagent --channel stable --update # Downgrade to Stable
```
Or type `/update` or `/channel beta` directly inside the TUI session. The updater performs atomic binary replacement in-place with zero downtime.

> [!NOTE]
> **And remember:** it is **not bug-free** — nothing is! I am a **solo developer**, and bugs are common in this project until more maintainers or supporting people start to contribute to it. If you want the most bug-free experience, please choose the **`stable`** branch (`flashagent --channel stable`).

---

## 🚀 Quick Start

### 1. Installation

#### One-Line Install (Linux & macOS)
```bash
curl -fsSL https://raw.githubusercontent.com/flashback7766/FlashAgent/main/install.sh | bash
```

#### One-Line Install (Windows PowerShell)
```powershell
irm https://raw.githubusercontent.com/flashback7766/FlashAgent/main/install.ps1 | iex
```
*Both installers automatically install missing extraction dependencies and add FlashAgent to your persistent `PATH`.*

#### Pre-Built Packages & Binaries
Download packages for your OS and architecture from [GitHub Releases](https://github.com/flashback7766/FlashAgent/releases):
- **Arch Linux**: `sudo pacman -U flashagent-bin-<version>-1-x86_64.pkg.tar.zst`
- **Debian / Ubuntu**: `sudo dpkg -i flashagent_<version>-1_amd64.deb`
- **Universal Linux AppImage**: `FlashAgent-<version>-x86_64.AppImage`
- **Generic Linux**: `flashagent-<version>-linux-x86_64.tar.gz`
- **macOS**: `flashagent-macos-<version>-aarch64.tar.gz` (Apple Silicon) / `flashagent-macos-<version>-x86_64.tar.gz` (Intel)
- **Windows**: `flashagent-windows-<version>-x86_64.zip`

#### Build From Source
Requires [Rust 1.85+](https://rustup.rs):
```bash
git clone https://github.com/flashback7766/FlashAgent.git
cd FlashAgent
cargo build --release
./target/release/flashagent-tui
```

---

### 2. Connect Your Model (Local or Cloud)

FlashAgent connects directly to any OpenAI-compatible server:

<details open>
<summary><b>Option A: LM Studio (Recommended Local Setup)</b></summary>

1. Open [LM Studio](https://lmstudio.ai) and load your desired model (e.g. `gemma-4-e2b-it-qat@q4_k_xl` or `Qwen2.5-Coder-32B`).
2. Start the local server on port `1234`.
3. Launch FlashAgent:
   ```bash
   flashagent
   ```
   FlashAgent automatically discovers LM Studio, queries loaded models, configures thinking tags, and connects instantly.
</details>

<details>
<summary><b>Option B: Ollama</b></summary>

```bash
ollama run qwen2.5-coder:32b
# In FlashAgent, press Tab -> Set endpoint: http://localhost:11434/v1
```
</details>

<details>
<summary><b>Option C: llama.cpp</b></summary>

```bash
llama-server -m models/qwen2.5-coder-32b.gguf --port 8080
# In FlashAgent, press Tab -> Set endpoint: http://localhost:8080/v1
```
</details>

<details>
<summary><b>Option D: Cloud Endpoints (OpenRouter, DeepSeek, OpenAI, Groq)</b></summary>

Configure your base URL and API key via `/settings` (or press <kbd>Tab</kbd> on launch):
- **Base URL**: `https://openrouter.ai/api/v1`
- **API Key**: `sk-or-v1-...`
- **Model**: `deepseek/deepseek-r1` or `anthropic/claude-3.7-sonnet`
</details>

---

## ⌨️ Keybindings & Controls

| Key Shortcut | Action Description |
| :--- | :--- |
| <kbd>Enter</kbd> | Send prompt / **Inject Mid-Flight Steering** during active generation |
| <kbd>Tab</kbd> | Open / Close Settings menu (or trigger command autocompletion) |
| <kbd>Shift</kbd> + <kbd>Tab</kbd> | Cycle permission mode (`Manual` ➔ `Planning` ➔ `Autonomic` ➔ `Bypass`) |
| <kbd>F1</kbd> | Inspect Context Window breakdown and token budget |
| <kbd>F2</kbd> | Cycle reasoning view mode (`collapsed` ➔ `last expanded` ➔ `all expanded`) |
| <kbd>F3</kbd> / <kbd>Ctrl</kbd> + <kbd>M</kbd> | Open Model selector with instant fuzzy search |
| <kbd>F4</kbd> / <kbd>Ctrl</kbd> + <kbd>T</kbd> | Open Thinking Effort preset menu (`off`, `low`, `med`, `high`, `max`) |
| <kbd>F5</kbd> | Open Sampling Parameters panel (`temperature`, `top_p`, `max_tokens`) |
| <kbd>Alt</kbd> + <kbd>O</kbd> | Toggle all thinking/reasoning blocks expanded or collapsed |
| <kbd>Ctrl</kbd> + <kbd>R</kbd> | Regenerate last assistant response from scratch |
| <kbd>Esc</kbd> | Close active menu / Dismiss suggestion / Interrupt active generation |
| <kbd>Ctrl</kbd> + <kbd>C</kbd> | Cancel current generation / Press twice to exit FlashAgent |

---

## 🏗️ Architecture

FlashAgent is organized as a modular, decoupled multi-crate Rust workspace:

```
FlashAgent Workspace
├── crates/core    # Agent loop, state machine, mid-flight steering, permissions, memory
├── crates/llm     # Universal OpenAI-compatible streaming engine & thinking parser
├── crates/tools   # Sandboxed shell executor, atomic diff patcher, ripgrep, web fetch
├── crates/tui     # High-performance crossterm TUI, prefill tracker, M3 expressive tail
├── crates/svc     # Background service layer & native atomic self-updater
├── crates/data    # SQLite + FTS5 persistent conversation & memory store
├── crates/proto   # Strongly-typed IPC schemas and serialization contracts
└── crates/ui      # Native wgpu hardware-accelerated renderer (GPU track)
```

Run test suite across all crates:
```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

---

## 🤝 Contributing

Contributions are warmly welcomed!
1. Read **[PHILOSOPHY.md](PHILOSOPHY.md)** for design principles and architectural ground rules.
2. Read **[CONTRIBUTING.md](CONTRIBUTING.md)** for our development workflow and coding standards.
3. Check **[ROADMAP.md](ROADMAP.md)** to see current and upcoming milestones.

---

## 📜 License

FlashAgent is released under the **[MIT License](LICENSE)**.

