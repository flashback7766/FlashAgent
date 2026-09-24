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
| `llm` | One model client and the protocols it speaks: OpenAI-compatible, Anthropic, Gemini, Ollama. Thinking-preset discovery, text tool-call parsing, JSON repair, token estimates. |
| `tools` | Built-in tools (files, patching, shell, search, git, memory, web), the MCP client and marketplace, subagent spawning. |
| `tui` | The terminal UI: chat view, composer, menus, settings, setup wizard, `/goal`, image attachments, what's-new screen. |
| `svc` | The self-updater. |

`core` does not depend on `tools` or `tui`; the loop talks to tools and to
the model through traits (`ToolExec`, `LlmSource`), which is also how the
tests drive it with mocks.

```mermaid
graph TD
    tui["tui — the flashagent binary"] --> core
    tui --> tools
    tui --> llm
    tui --> svc
    tools --> core
    tools --> llm
    svc --> core
    core --> llm
```

## One turn

```mermaid
sequenceDiagram
    actor User
    participant TUI as tui (App)
    participant Loop as core::AgentLoop
    participant Model as llm::Client
    participant Gate as core::PermissionedTools
    participant Tools as tools::BuiltinTools

    User->>TUI: Enter
    TUI->>Loop: spawn the turn (history, budgets, cancel token)
    loop until the model answers without a tool call
        Loop->>Model: stream the history
        Model-->>Loop: text, reasoning, tool calls
        Loop-->>TUI: LoopEvent per delta
        Loop->>Gate: each tool call
        alt the mode allows it
            Gate->>Tools: run
        else it needs a person
            Gate-->>TUI: approval card
            User->>TUI: allow / always / deny
            TUI->>Gate: the answer
            Gate->>Tools: run, or refuse
        end
        Tools-->>Loop: result, fed back to the model
    end
    Loop-->>TUI: Done
    TUI->>TUI: save the session
```

The loop runs on its own task. The UI never waits on it: events arrive
through a channel and are drawn when they come. Enter while a turn runs
sends a steering message down another channel into the same turn.

`run_shell` with `background: true`, or a foreground command the user moves
there with Ctrl+B, becomes a task in `tools::shell::ShellRegistry`, whose
watcher sends one notice when it exits. The app hands the notice to the
model (`core::NoticeInbox` makes sure it arrives once): down the steering
channel into a running turn, or as a follow-up turn when the app is idle
and no approval, question or typed draft is open, and not after the user
stopped a turn with Esc. A task killed on purpose wakes nobody. `/tasks`
lists them; quitting stops them.

## Agent loop

`core::loop_` streams a turn from the model, collects tool calls (native, or
parsed out of text for models that write them inline), runs each call
through the permission layer, feeds results back, and repeats until the
model answers without a call or a budget runs out. Budgets: steps, total
tokens, generated tokens, wall-clock time. Every event (text, reasoning,
tool start and finish, usage) is emitted for the UI.

Every tool call the model makes gets a result, including when the user
cancels, because strict servers reject a history with an unanswered call.

`run_shell` uses `sh` on Unix. On Windows it uses Git Bash when Git for
Windows is installed, since models write Unix commands and cmd.exe runs
none of them; otherwise cmd.exe, and the system prompt names the shell
either way. Output that is not UTF-8 is decoded from the console's OEM code
page, which is what cmd.exe and most Windows tools write to a pipe.

## Permissions

Four modes: Planning (read-only; shell commands on a short list of programs
that only read run, everything else is refused), Manual (ask before anything
that is not a read), Accept Edits (file changes allowed, shell asks), Accept
All. Shell approvals can be remembered per command prefix, and a prefix never
covers a different subcommand: approving `npm test` does not approve
`npm publish`. A file outside the project asks in Manual and Accept Edits
and is refused in Planning and during `/goal`; Accept All lets it through,
since its shell already reaches any file. A short blacklist of dangerous commands (recursive force
deletes, `sudo`, force pushes, hard resets, ...) still asks in Accept All and
is refused outright during `/goal`, where nobody is there to answer. A
declined call is returned to the model as an error it must not retry. The
mode the user is in is saved as the one to start in next time.

## Model backend

`llm::Client` holds what outlives a request (the endpoint, the model, what
the server was found to support and to refuse) and hands each request to
the module for the protocol its endpoint speaks:

| Protocol | Speaks to |
|---|---|
| OpenAI-compatible (`/chat/completions`) | LM Studio, llama.cpp, vLLM, Jan, KoboldCpp, text-generation-webui, LocalAI, OpenAI, OpenRouter, DeepSeek, Mistral, Groq, xAI, Together, Fireworks, Cerebras, any other copy |
| Anthropic (`/v1/messages`) | Anthropic |
| Gemini (`generateContent`) | Google Gemini |
| Ollama (`/api/chat`) | Ollama, which can set the context window there and not on its `/v1` |

The protocol is chosen with the provider, never guessed per request: one
address can speak two (Ollama answers both). An address typed by hand gets
`ApiProtocol::detect`, which recognises only hosts that speak one protocol
and takes anything else as OpenAI-compatible. What a model supports (context
window, tool use, vision, thinking presets) is read from the server; the
thinking profile is re-read when the model changes.

**Providers.** `config.json` keeps a list of providers (`ProviderProfile`
in `core::config`: name, protocol, address, key, model) and the name of the
one in use; `AppConfig::endpoint()` is that one's. The presets
(`BackendPreset`) carry their protocol and the environment variables a key
is looked for in. A provider with no saved key uses the first of those that
is set, then `FLASHAGENT_API_KEY`. A config from before providers
(`backend_url`, `api_key`, `model`) is read as one provider named after its
server's preset; saving writes those three fields too, from a provider the
OpenAI protocol reaches, so an older build still finds a server, but they
are never read back while `providers` exists. `--url` is a provider for the
one run, never saved. An Ollama provider can set the context window it runs
with (`num_ctx`); otherwise FlashAgent picks it once per model and keeps it,
since a change makes Ollama load the model again.

**Sampling.** The presets (Coding, Gemma, ...) are tuned for local models.
Anthropic and Gemini get none of them unless the user set sampling by hand
(Custom): their makers tune their own models, and Gemini 3 loops below
temperature 1.0. `Client::set_user_sampling` applies this to every request.

**Switching.** `/provider`, Settings and the setup wizard change the provider
while the app runs (`provider_switch.rs`): `Client::set_endpoint` drops what
was learned about the old server, discovery asks the new one, and the model
saved for the provider is taken up if the server lists it, else the loaded
or first one. The context window, thinking profile, vision, effort bias,
model list, system prompt and welcome card follow the model, and the new
server's prompt cache is warmed. The history is protocol-neutral, so the
conversation carries on. A switch is refused while a turn runs. Discovery
settles its answer into whatever endpoint the client has when it ends, so a
switch waits for a look already under way, and of two quick switches only
the later one changes the client.

A local server reuses the part of a request it has already read, so each
request keeps its opening fixed: system prompt, voice example, history, and
only then anything temporary. The first message would still start cold, so
`warm.rs` sends that opening ahead, with a one-token answer, while the user
types: at start, after `--resume`, after a change of model, provider or
voice. It is skipped when a turn has just read the same prefix from the
same server, and for a hosted API, which bills it and reads a prompt in a
second anyway: only Ollama, a server that runs local models, or one at a
local address is warmed.

## Memory

- Rule files: `MEMORY.md`, `CLAUDE.md`, `AGENTS.md` and `.agents/rules/` in
  the project, and `~/.flashagent/` globally, injected into the prompt.
- Remembered facts: one file per fact under `memory/`, indexed by
  `MEMORY.md`. Facts about the user are global, facts about the code stay
  with the project. Writes are refused during `/goal`.

## Terminal UI

The app runs on the alternate screen. `render.rs` builds each frame as the
rows that should be on it: the end of the transcript, then the "tail" (the
turn in progress, the composer and the footer). `screen::Screen` compares
them with the rows already on the terminal and writes only those that
changed, each over the old one by absolute position. Nothing is cleared
first, so a terminal that shows a frame half-written shows old rows beside
new ones, never a blank screen. On Windows the frame goes to the console in
one `WriteConsoleW` call, since the standard library splits console output
into pieces of about 2 KB and the console draws between them. The setup
wizard, the release notes and the trust question draw through the same
`Screen`.

```mermaid
graph LR
    E[LoopEvent] --> C[ChatView lines]
    C -->|render_split| S[settled rows, shared]
    C -->|render_split| L[live rows]
    S --> V[viewport: last screenful]
    L --> T[tail: live + composer + footer] --> V
    V --> D[Screen: changed rows only] --> R[theme recolour] --> O[terminal]
```

- `ChatView::render_split` keeps the settled rows in a cache shared through
  an `Arc`. A frame reads again only from the turn the settled boundary
  falls in, so its cost barely grows with the length of the session (see
  [docs/numbers.md](docs/numbers.md)).
- Every row is measured in display cells and clipped or wrapped to the
  width. A row wider than the terminal would wrap and push every row under
  it down. The bottom row stays one column short, because a character in
  the last cell makes some consoles scroll the screen.
- The conversation scrolls; the composer and the footer under it do not.
  Scrolled back, the view stays on the same lines while new ones arrive.
- `ChatView::render_split` also records which conversation line each row
  came from, so a click on a thought or a tool call opens or folds that one
  (`ChatLine::is_expanded` overrides F2 for it).
- Nothing moves the layout while it animates: the welcome card holds its
  full height while it draws itself in, and the tip line keeps the same
  number of rows for every tip at a given width.
- Every symbol the UI draws is in both Consolas and Cascadia Mono: a Windows
  console outside Windows Terminal falls back to no other font and shows a
  missing one as a box. `crates/tui/tests/glyphs.rs` checks the source.
- Shortcuts are read by the key, not the letter: with a Russian layout
  Ctrl+D arrives as Ctrl+в and is turned back into Ctrl+D. A character the
  layout lacks (`{` on a Russian one) comes from the console as an Alt code,
  a key release with no press before it, and is taken as typed.
- Themes are applied to the finished frame by rewriting its 24-bit colour
  sequences, so the drawing code has one palette.
- Motion (Settings → Animations) goes through `anim::enabled()`. Off, only
  the spinner moves.
- The composer is `composer::Composer`: text and a cursor that steps over
  grapheme clusters. It folds its text into rows for the box, and the box
  grows up to a third of the screen, then scrolls around the cursor.

## Sessions

A conversation is saved after each turn and on exit, to
`~/.flashagent/sessions/<id>.json`.

- The id is `session_<unix seconds>_<process id>`. Two instances running at
  once never share a process id, so they never share a session file.
- A save writes a temporary file beside the target and renames it over
  the target. A crash leaves the old session or the new one, never half.
- A failed save is shown on screen and retried after the next turn. On
  exit there is no next turn, so the conversation goes to the temporary
  folder instead, and the message says where.
- A `--resume` whose file cannot be read starts a new session under a new
  id. The damaged file is left as it is.

## Storage

Plain files under `~/.flashagent/`: `config.json` (settings and the saved
providers, keys included), `sessions/*.json`,
`snapshots/<session>/` (file copies for `/rewind`), `prompt_history.jsonl`
(what was sent, for ↑ and Ctrl+F; kept only while sessions are saved),
`memory/` and `MEMORY.md`, `rules/`, `skills/`, `mcp.json`, `effort.json`,
`image-costs.json`, `prefill_cache.json`. A project can add its own
`.mcp.json`, `.agents/rules/` and `.agents/skills/`.

## Updates

GitHub Releases, `beta` and `stable` channels. Version formats (`b287`,
`v1.0.0+b290`), how they compare, and how to release are in
[VERSIONING.md](VERSIONING.md). The updater downloads the
release asset, checks it against `SHA256SUMS`, and replaces the binary; the
new version runs on the next start.

## Tests

- Unit tests next to the code.
- Loop tests against a mock model.
- Render tests that assert no row overflows the terminal width.
- `crates/tools/tests/every_tool.rs` calls every built-in tool for real in a
  temporary project.
- `crates/tui/tests/scenarios.rs` runs the real binary in a pseudo-terminal
  (`portable-pty` + `vt100`) against a scripted model server, types into
  it, and reads the screen.

CI runs `cargo test --workspace` and `cargo clippy --all-targets
-D warnings` on Linux, Windows and macOS, and the same run gates every
release. The tests that need the internet run nightly
(`.github/workflows/web-tools.yml`).
