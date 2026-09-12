# Architecture

What exists today. Plans live in [ROADMAP.md](ROADMAP.md).

## Process

One process. `flashagent` (crate `flashagent-tui`) runs the terminal UI, the
agent loop, the tools and the permission layer together. There is no
background service and no IPC.

## Crates

| Crate | What it holds |
|---|---|
| `core` | Agent loop and its events, permission modes and approval gating, subagents, memory (rule files and the per-fact store), system prompt, config, auto-effort learning, tool-calling check. |
| `llm` | One backend: an OpenAI-compatible streaming client. Thinking-preset discovery, text tool-call parsing, JSON repair, token estimates. |
| `tools` | Built-in tools (files, patching, shell, search, git, memory, web), the MCP client and marketplace, subagent spawning. |
| `tui` | The terminal UI: chat view, composer, menus, settings, setup wizard, `/goal`, image attachments, what's-new screen. |
| `svc` | The self-updater. |
| `proto` | Empty. Reserved for a process split that has not happened. |

`core` does not depend on `tools` or `tui`; the loop talks to tools and to
the model through traits (`ToolExec`, `LlmSource`), which is also how the
tests drive it with mocks.

## Agent loop

`core::loop_` streams a turn from the model, collects tool calls (native, or
parsed out of text for models that write them inline), runs each call
through the permission layer, feeds results back, and repeats until the
model answers without a call or a budget runs out. Budgets: steps, total
tokens, generated tokens, wall-clock time. Every event (text, reasoning,
tool start and finish, usage) is emitted for the UI.

Every tool call the model makes gets a result, including when the user
cancels, because strict servers reject a history with an unanswered call.

## Permissions

Four modes: Planning (read-only), Manual (ask before anything that is not a
read), Accept Edits (file changes allowed, shell asks), Accept All. Shell
approvals can be remembered per command prefix, and a prefix never covers a
different subcommand: approving `npm test` does not approve `npm publish`.
A declined call is returned to the model as an error it must not retry.

## Model backend

Anything that speaks the OpenAI chat-completions API: LM Studio, Ollama,
llama.cpp, vLLM, OpenRouter. What a model supports (context window, tool
use, vision, thinking presets) is read from the server; the thinking
profile is re-read when the model changes.

## Memory

- Rule files: `MEMORY.md`, `CLAUDE.md`, `AGENTS.md` and `.agents/rules/` in
  the project, and `~/.flashagent/` globally, injected into the prompt.
- Remembered facts: one file per fact under `memory/`, indexed by
  `MEMORY.md`. Facts about the user are global, facts about the code stay
  with the project. Writes are refused during `/goal`.

## Storage

Plain files under `~/.flashagent/`: `config.json`, `sessions/*.json`,
`memory/` and `MEMORY.md`, `rules/`, `skills/`, `mcp.json`, `effort.json`,
`image-costs.json`, `prefill_cache.json`. A project can add its own
`.mcp.json`, `.agents/rules/` and `.agents/skills/`.

## Updates

GitHub Releases, `beta` and `stable` channels. The updater downloads the
release asset, checks it against `SHA256SUMS`, and replaces the binary; the
new version runs on the next start.

## Tests

Unit tests next to the code, loop tests against a mock model, and render
tests that assert no row overflows the terminal width. CI runs
`cargo test` and `cargo clippy -D warnings`.
