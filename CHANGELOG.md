# Changelog

## b261 — the model you loaded

- The setup wizard and the model list find the model LM Studio has loaded.
  LM Studio's `/api/v1/models` can leave models out — on a real server it left
  out the loaded `qwen3.6-35b-a3b-mtp` — and FlashAgent stopped at that list, so
  step 3 offered three idle models and called them *3 models loaded*. It now
  fills the gaps from `/api/v0/models`, which lists every model with its state.
  Checked against LM Studio: step 3 shows *qwen3.6-35b-a3b-mtp · ● loaded ·
  vision*.
- An embedding model is no longer offered as the model to talk to.
- The model count says what it counts: *3 on the server*, not *3 models
  loaded*.

## b260 — The slop is human

Humans can make far more slop than AI ever will. All it takes is not watching
the AI.

A lot of people think "vibecoded" means "AI slop". I was tired of hearing it,
and more tired of catching myself thinking it. So, plainly: FlashAgent is
written with Anthropic's Claude, and every commit says so. I don't understand
every line yet. What I do is watch. I decide what gets built, run it every day
against local models, break it, and send back screenshots until it works.

The slop that was in this repo wasn't the AI's. It was the part nobody checked:
a README banner with numbers nobody measured, docs describing a GPU renderer
that never existed, two thousand tips of random trivia, crates nothing used,
and a 7,236-line main file with a 3,349-line function inside it. That one is on
me, not on Claude.

So this release is me watching. That function is 982 lines now and the file is
2,293; forty real tips are left; every number in the README was measured. What
the app does didn't change: 462 tests, clippy with warnings as errors, and a
live run against a real model after every step.

Still see slop? Open an issue and point at the line. That is worth more than
"lol vibecoder".

— flashback

<!-- page -->

### Less slop

- `main.rs` is 2,293 lines instead of 7,236, and its event loop 982 instead of
  3,349. Keys, prompt submission, turn completion, rendering, sessions and menus
  are modules of their own, and the chat library is split the same way. Nothing
  it does changed; a live run against a real model after each step checked it.
- Forty tips about FlashAgent instead of two thousand about everything, and a
  test fails if a tip names a command that does not exist.
- The crates nothing used are gone, the empty protocol crate last, along with
  dependencies nobody imported.
- The docs describe what exists. The architecture page named a GPU renderer, an
  IPC service and telemetry that were never built, and the README banner
  promised a cold start nobody had measured.
- The tests run on Windows again. One test wrote a Windows path into JSON by
  hand, the backslashes broke it, and since b249 that failure stopped every
  test after it. CI now runs every suite even when one fails.

<!-- page -->

### Talking to the model

- The auto-compaction threshold follows the window: 97% at a million tokens,
  95% at 512k, 90% at 256k, 85% at 128k, 80% at 64k, 75% at 32k and below. One
  percentage cannot suit both ends — ten percent of a million tokens is a
  hundred thousand left empty, while ten percent of 32k is not one answer.
  Settings says which one is in force: *Enabled (auto: 85% for this window)*.
  A threshold you set by hand is still yours; the old shipped default of 90 was
  never a choice, so it becomes automatic.
- A finished thought no longer says it is still thinking. The block above a
  written answer read *Thinking: …* for the rest of the session; it reads
  *Thought: … (4s)* now, expanded or collapsed, and only says Thinking while
  the tokens are still arriving.
- The labels FlashAgent writes follow the conversation in **both** directions.
  Detection only ever switched to Russian, so an English chat on a config that
  said `ru` was labelled *Разбираю запрос* and nothing typed could change it
  back.
- The suggestion list no longer hangs on the screen while the context is being
  compacted. The command that opened it is long gone from the composer.

<!-- page -->

### The first five minutes

Walked from an empty home directory with the published build, as a new user
gets it, and fixed what that turned up.

- *"Welcome back"* greeted people who had never been here. It now says
  *Welcome* until there is a saved session to come back to.
- The interface language was Russian for everybody, because that was the
  shipped default and the wizard never asks. It follows the locale now, and
  what you type still overrides it.
- The tool-calling check printed its verdict to a screen the app then
  cleared, so the one thing a new user needed to read was gone before they
  could read it. It is now the first line of the conversation: *Tool-calling
  check: 6/8 — unreliable — expect to babysit it.*
- That check ran before the "do you trust this directory?" question, so the
  first thing a new user did was wait a minute and only then get asked where
  they were. The instant question comes first.

<!-- page -->

### Smaller things

- Approval cards name files relative to the project. An edit showed
  `target: /tmp/.../project/main.rs` and a diff starting `--- a//tmp/...`; it
  now shows `target: main.rs`, and the six preview rows go to the change itself
  instead of repeating the file name.
- The what's-new screen can open a release with a note, and a note is not
  skipped by accident: it takes three presses to move past it. This one, for
  example.
- On Windows the prefill-time cache is saved. It looked for the home directory
  only in `HOME`, which Windows does not set.
- Long notes read properly here: wrapped lines no longer start with a space,
  *emphasis* is italic, a heading gets a blank row above it, and Tab keeps you
  on the entry you were reading.

## b250 — compaction you can watch, and what a picture costs

- Compaction says what it is doing, in the chat, where it belongs: *Compacting
  context...* becomes *Context compacted · 12K saved · the conversation so far
  is now a summary*. It changes what the model remembers, so it is part of the
  conversation and not a notice that fades.
- **Compaction was quietly failing on any model worth using.** The summary had
  twelve seconds to arrive and five between chunks; a 35B writing at ten tokens
  a second never finished, so every compaction fell back to a list of truncated
  snippets. The budget now fits a real model, and the summary is asked for by
  section — goal, decisions, files, facts, state, next — so what the next turn
  needs cannot be summarised away. Checked end to end: after compaction the
  model still answered with a port number and a branch name that existed only
  in the part that was summarised.
- The auto-compaction threshold looks at more than a percentage. It compacts
  when the next turn would not fit — sized from what the last turn cost —
  rather than waiting for a line to be crossed and discovering the overflow
  mid-turn; and it does not compact a conversation whose bulk is the system
  prompt and tool schemas, where summarising frees nothing and costs the recent
  context.
- The attachment row says what a picture will cost: `attached screenshot
  1440×900 · ~1.2k tokens`. The number is measured against the server once per
  model, because it cannot be guessed — a Qwen charges by area (about a token
  per thousand pixels, measured), a Gemma a flat rate per image.
- No emoji in the attachment row.
- The what's-new screen shows the changelog's emphasis as emphasis. It printed
  `**Pictures.**` with the asterisks.

## b249 — pictures

- `view_image` — the model can open a picture in the project itself: a diagram
  in `docs/`, a screenshot in a bug report, a mockup to build from. The picture
  comes back as a picture, in a message of its own, because a tool result is
  text on every server worth supporting. Offered only to models that can see,
  refuses anything outside the working directory, and refuses a file too large
  to send with its size rather than failing the turn. Verified: qwen3.6-35b
  opened a diagram and described the two boxes and the arrow between them.
- A file named in a sentence stays a mention; only a written-out path attaches
  itself. Otherwise asking "what is in diagram.png?" quietly sent a megabyte.
- **Pictures.** `Ctrl+V` pastes a screenshot straight from the clipboard into
  the message — the composer shows `📎 screenshot 1440×900`, `Ctrl+Z` takes it
  back, and the model sees it. Dropping an image file on the window works too,
  as does naming one in the text. Verified end to end: a drawn digit pasted
  into a chat with qwen3.6-35b came back as "7".
  A model that cannot see is named as such when you attach, rather than letting
  you send into the dark, and pictures survive `--resume`.
- Every tool call now explains itself, reads and searches included. The header
  was recommended but optional for them, and a capable 35B model wrote one
  exactly never — so a search read `Searched "**/*.rs"` while an edit read like
  a sentence. It is asked for on every call now: `Find all .rs files in the
  project · "**/*.rs"`, `Read main.rs contents`.
- Paths inside the project are shown relative to it. A line reading `Read
  /tmp/claude-1000/-home-flashback/.../audit_proj/main.rs` was all prefix and
  no information.
Found by running every surface of the app against a real model and reading
what the server actually received.

- Fixed: switching models kept the previous model's thinking profile, so
  FlashAgent asked a model that cannot reason for a reasoning effort. LM Studio
  logged *"'minimal' reasoning effort is not directly supported"* on every
  turn. A profile belongs to a model, and is now re-derived when the model
  changes; a model discovery knows nothing about gets no thinking fields at all.
- Fixed: a model switch also overwrote your effort setting — `auto` became
  `off` the moment the server loaded a model without reasoning, and stayed off
  for every model afterwards. The setting is yours; `/effort` now says "this
  model does not reason; kept for the next one" instead of silently changing it.
- A turn parked on an approval card said "Generating response...". It is not
  generating anything; it says **Waiting for your answer**.
- Esc on an empty prompt quit immediately. It now asks, and the session is
  saved either way.
- Tool cards keep the facts next to the model's header: `Create note.txt with
  text hello · note.txt +1`. The header said what it meant to do; only the card
  says what happened to the tree. A header that already names the file gets
  only the part it left out.
- A declined tool call told the model "denied by user", which one model read as
  a privilege error and started asking for sudo. It now says the user declined,
  and not to retry or work around it.
- `/diff` said "working tree clean" with six untracked files in the directory.
  It now says what it actually checked: tracked files, and how many untracked
  ones it did not.
- `/export` wrote `session_session_1789.md`.
- Skills are listed by their own `description:`, not "Skill defined in greet.md".
- A resumed `/goal` showed its whole internal directive as if you had typed it.

## b246 — memory that survives the session

- The what's-new screen opens with the claims, not the whole text: one line
  per change, with `…` where there is more and `tab` to read it. Five
  paragraphs at once is a wall nobody reads.
- Memory the model keeps for itself. One fact per file under `memory/`, with
  `MEMORY.md` as an index of one line each — so what is remembered no longer
  grows until it crowds out the conversation it was meant to help. Every fact
  carries its type (preference, decision, reference, work) and the date it was
  written, because a note that does not say when it was true quietly outlives
  the decision it recorded. Facts about you go global and follow you into every
  project and every model; facts about the codebase stay with the project.
- The model now writes memories without being asked. The tools existed before
  this, but nothing in the prompt said what was worth remembering, so it never
  used them. It is told now — corrections you gave it, decisions and their
  reasons, commands that are not discoverable — and told what is NOT worth it:
  anything the code, git history or README already says.
- `/memory` shows everything remembered, in both scopes: `d` forgets one, and
  `e` sends the model a note about the one under the cursor — "this is out of
  date, I use just test now" — and it corrects the memory itself. Memory
  written without being asked has to be visible and reversible.
- Writing a memory is announced on the line under the input, with a pointer to
  `/memory`.
- Fixed: the what's-new screen did not appear for anyone updating into b245.
  Nobody had a recorded version yet — that field ships in b245 — and an absent
  version was read as a first run. An existing config with no version is now
  read as what it is: someone who updated, and has news to read.

Beta builds are numbered `bNNN` and ship on the rolling `beta` pre-release.
Stable versions start at `v1.0.0` and ship on the rolling `stable` release.

## b245 — auto effort that learns, and a screen that speaks your language

- Auto effort learns from how the turns actually go. The guess about a task is
  made before the model has said a word, so it cannot know that *this* model
  thinks for two minutes about a one-line answer, or that it fumbles tool calls
  unless given room. Now the turns are watched — a failed tool call or a
  Ctrl+R asks for more thinking, a mountain of reasoning with a sentence of
  answer and nothing done asks for less, stopping it mid-thought counts as too
  slow — and the level is nudged by at most one preset step, only after three
  consistent turns, fading back to neutral as soon as the turns stop
  complaining. Learned per model, kept in `~/.flashagent/effort.json`, and
  `/effort` says what it settled on: *Auto (per turn; learned one step down,
  after 7 turns)*. A preset you picked by hand is never second-guessed.
- Fixed: the recap under a turn often never appeared. The request was capped at
  512 tokens, and a model told not to think thinks anyway — one run spent 361
  of them reasoning and was cut off before it closed its JSON, so nothing was
  shown. The budget now covers that, the recap is asked for first so it
  survives a cut-off, and a sentence that did finish is kept even when the
  brace after it never arrived.
- Fixed: a Russian chat was labelled in English — `Thought: Analyzing Request`,
  `Planning Implementation`. Those names are FlashAgent's own, and they now
  follow the conversation: `Разбираю запрос`, `Планирую реализацию`. A stage
  lifted out of the model's own reasoning is still shown exactly as it wrote
  it — translating a quote would put words in its mouth.
- Fixed: listing the working directory read `Searched search`. It now reads
  `Searched the project`.

- After an update, FlashAgent shows what arrived. One release per screen,
  entries appearing one at a time, `enter` to move on and `esc` to skip the
  rest — read straight out of this file, so the screen cannot claim a feature
  that did not ship. It appears exactly once per version, never on a first
  run, and `/whatsnew` reopens it whenever you want. A fresh install is
  stamped silently, so nobody is greeted by a year of history.

- Narrow terminals no longer cut text in half. Lines built from several facts
  now drop whole facts when the width runs out — `gemma-4-e2b · auto · 64k`
  becomes `gemma-4-e2b · auto`, not `gemma-4-e2b · auto [off` — the composer
  shortens its prompt, and a tip too long for two lines ends on a word. A
  test renders the welcome card at six widths down to 30 columns and fails if
  any row overflows.
- Short windows show the real mascot again. They had their own hand-drawn
  "mini" version that still had the old round eyes, so the creature changed
  species when the window got short.

- Backend failures are explained instead of dumped. `llm: http: error sending
  request for url (http://localhost:1234/v1/chat/completions)` now reads *No
  model server answered at http://localhost:1234/v1*, with a line saying what
  to do and the original text kept underneath. A missing model points at F3, a
  full context at `/compact`, a rejected key at the settings, a dropped stream
  at the server's own log. Anything unrecognised keeps its own words rather
  than being smoothed into a confident guess.
- An unreachable model server says so when you start, not when you send your
  first prompt — and takes it back by itself once the server answers.
- Auto reasoning effort judges the task instead of the vocabulary: it used to
  match forty English keywords, so Russian prompts matched none of them, and
  it measured length in bytes, which made every Cyrillic sentence "long" and
  sent it to maximum reasoning.

- The composer says what the turn is actually doing instead of "Working on
  task": waiting for the model, thinking, running the tool the model named,
  reading the result, writing the answer, stopping. Every one of those comes
  from something the loop reported — there is deliberately no "almost done",
  because the program does not know that.

- Tool calls now carry a one-line explanation the model writes itself, and
  that line is what you read: `Add the missing null check to parser.rs`
  instead of `Editing parser.rs`. It is required for anything with
  consequences — writes, patches, shell commands — and recommended for reads
  and searches, so the cheap calls stay cheap. When a call fails, the same
  sentence says so: `Failed to add the missing null check to parser.rs`. The
  expanded card underneath is unchanged: the explanation is the model's
  stated intent, the card is what actually happened.

- Changing the release channel now asks first. It replaces the binary with a
  different line of builds, so the card says what that means — moving down to
  stable can take features away and reset the settings they introduced — and
  names the version you would land on. Answering no puts back only the
  channel: everything else you changed in the same visit stays applied. If the
  channel you picked has nothing published on it yet, it says that instead of
  pretending there is a version waiting.

- Messages that appear on their own — an update installing, the context being
  compacted — now live on the line under the input rather than in the composer.
  The composer answers what you just did; something you did not ask for must
  not take that spot. They also share the line with the live token counters
  while a turn is running, instead of waiting for it to finish, and the ones
  that stop being actionable dim out over their last second rather than
  blinking away.
- The recap and the follow-up suggestion now come back in the language of the
  conversation. "Write it in the language of the conversation" is not an
  instruction a small model follows; the language is now named outright,
  detected by script — and a Russian conversation that quotes Python still
  reads as Russian.

- `flashagent --tool-test` scores your model on eight scenarios the agent loop
  performs on real work, and `--all-models` does it for every chat model your
  server lists and prints a markdown table. The check also runs once after
  setup, so a model that cannot drive tools is something you learn in the first
  minute rather than the first hour. Results for four local models, the method
  and the raw output are in [docs/tool-calling.md](docs/tool-calling.md).

- One-line system notices moved out of the transcript and under the cursor.
  `[No models discovered]`, `[Usage: /verbose …]`, `Update b238 · [███░░] 52%`
  and the rest are addressed to the person at the keyboard, not to the
  conversation, and a chat full of them is a chat you stop reading. Anything
  with structure — a skills listing, a git diff, the `/goal` report — still
  belongs in the transcript and stays there.
- A finished update no longer says the same thing twice: the progress line
  disappears and the banner states the outcome.

- A single bad line in `config.json` no longer resets every setting. The
  loader used to throw the whole file away on any error, so one typo silently
  put you back on the default backend, model and permission mode. Now each
  field stands on its own: the broken ones fall back and are named on stderr,
  everything else is kept.
- Stored values are read whatever their case. `"Beta"`, `"beta"`,
  `"Accept Edits"` (the label the app itself shows) and `"accept_edits"` all
  mean what they look like — a config is edited by hand, and a capital letter
  was costing people their settings.
- CI keeps its build artifacts for three days instead of the default ninety.
  They only exist to hand binaries from the build jobs to the publish job in
  the same run — the release assets are the lasting copy — and two days of
  releases had filled the account's Actions storage with 2.3 GB of them.
- The README recordings are redone against the current build, and two were
  added where a still picture explains nothing: `/goal` being stopped by its
  step budget and handing back a report, and Ctrl+U downloading and installing
  an update. Each one is a real session against a local model, so the timings
  and token counts on screen are the ones it produced.

## b238 — no emoji in the tool cards

- Tool cards drop their emoji icons. `Edited notes.md +1 -0`, `Read
  src/main.rs (120 lines)`, `Analyzed src/` — the extension already says what
  a file is, so the icon carried nothing, and being two columns wide next to
  one-column glyphs it made listings fail to line up. Directories still end in
  `/`, the way `ls -F` marks them. A test keeps them out.

## b237 — no lightning left

- The last two lightning bolts are gone: the welcome card says `FlashAgent
  Engine` plainly, and a shell script is marked `$` rather than an emoji —
  which is also one column wide instead of two.

## b236 — updates you can watch

- The status line drops the lightning emoji from its numbers: `TTFT 6.84s
  (495 t/s prefill)`, `Prefill ~1.2s`, `cache 62%`. A telemetry row is meant
  to be read, not decorated.
- Ctrl+U (and `/update`) now checks, downloads and installs in one go, with
  the download visible: `Update b235 · [████████░░░░░░░░] 52% · 3.4/6.6 MB`,
  then `verifying checksum...`, `installing...`, and the line lands on
  `installed · restart FlashAgent to run it`. It rewrites one line rather than
  stacking up, and a background update stays silent as before.
- Fixed: an installed `flashagent` refused to update while you stood in the
  FlashAgent repository, claiming to be a dev build. What decides that is
  where the binary lives, not where you are — and the repository is exactly
  where you are when you notice there is a new build.
- Fixed: opening FlashAgent and closing it without saying anything saved a
  session and offered a `--resume` id that restored nothing. The history is
  never really empty — it opens with the system prompt — so the check now
  looks for an actual user message.

## b235 — budgets, a mascot that means something

- While the model works, the mascot's face sits in the status line —
  `(•_•) [Manual] · Generating response...` — blinking and winking on the
  turn's own clock. The welcome card is gone by then, so this is the only
  place it can keep you company.
- Fixed: `--resume` drew only the top border of the welcome card. The card is
  animated in row by row on start-up, and a resumed session prints its
  messages over it immediately, which stopped the animation halfway. A resumed
  session now gets the whole card at once.
- The follow-up suggestion above the input box no longer offers prompts the
  assistant meant for you ("Расскажи о своём проекте и опиши, в чём нужна
  помощь"). Pressing → sends the suggestion to the model, so a question aimed
  at you was worse than no suggestion at all; those are now dropped, and the
  analyzer is told plainly to write a message the assistant can answer on its
  own, in the language of the conversation. The giveaway is the object, not
  the verb — "расскажи об архитектуре" still passes.
- The mascot is drawn properly. A terminal cell is twice as tall as it is
  wide, so the old sprite — one pixel per cell — came out as a stretched,
  spiky kite. It now draws two pixels per cell, which makes the pixels square
  and the shape round. It also breathes (the highlight rises and falls over
  about three seconds), and the welcome card draws itself in from the top over
  half a second on start-up.
- The mascot's face reports whether the model server answered: eyes open and a
  smile when it did, eyes shut and drained colour when it did not, neutral
  while the first check is still running. Discovery reruns every few seconds,
  so starting your server turns the face around by itself — on a first run
  that is the difference between "nothing happens" and knowing why.
- `/goal` takes budgets: `/goal [--steps N] [--time 30m] [--tokens 200k]
  <task>`, defaulting to 250 steps and one hour. `--tokens` counts generated
  tokens only — with a prefix cache, a total-token budget mostly measures how
  long the conversation is. The status line shows the burn-down live
  (`step 12/250 · 4.2k tok · 3m05s/1h0m`), and the run ends with a report card
  built from loop events, not from the model's own account: steps, generated
  tokens, elapsed, files created and edited, shell commands that failed, and
  an explicit INCOMPLETE when a budget (or Esc) ended the run.
- The command is just `flashagent`. Packages, tarballs, the AppImage and the
  installers no longer add a `flashagent-tui` alias (reinstalling removes the
  old one); in-app hints use the new name. The empty `crates/app` is gone.
- External (MCP) tools run without approval only when your `.mcp.json` marks
  them `read_only` / lists them in `read_only_tools`. Tool names like `get_*`
  and the server's own `readOnlyHint` no longer skip the approval card.

## b233 — end-to-end audit

A full audit that traced every user-visible feature from key press to model,
tool and screen, then fixed what did not hold up. Every fix has a regression
test; the whole stack was also exercised live against LM Studio
(`gemma-4-e2b-it-qat@q4_k_xl`) in a real terminal.

### Tool calling and the agent loop
- Tool calls that models write as text (`<tool_call>`, `[TOOL_CALLS]`, bare
  JSON) are now executed. The parser existed but was never wired in; it only
  accepts names of advertised tools, never looks inside code fences (tracked
  across streamed chunks), runs every call of a Mistral array, and never
  "completes" a block that was cut off.
- Interrupting a turn no longer corrupts the conversation: every recorded tool
  call gets a result (`cancelled by user`) and partial answer text is kept, so
  the next request is valid and the model knows what happened.
- Servers that omit or repeat tool-call ids get unique 9-character ids (the
  shape Mistral templates require); nameless calls are dropped.
- A backend error mid-turn no longer discards the steps that already ran;
  a turn that ends without any reply is closed so strict chat templates
  (Gemma, Mistral) never see two user messages in a row.
- The stall-recovery nudge no longer leaks into history as a fake user message.
- Tool calls cut off by the output token limit are no longer executed (JSON
  repair could "complete" a truncated path or command into a different one);
  the model is told to resend them.

### LLM backend
- A persistent HTTP 400 recursed until the stack overflowed and the app
  aborted. Adaptation is now bounded: learn thinking presets only from errors
  about thinking, then fall back to fewer optional fields, then give up.
- Errors a server reports inside the stream (e.g. context overflow) surface as
  errors instead of an empty answer.
- The 10-minute hard request timeout is replaced by connect + idle timeouts, so
  long generations on slow local models are no longer cut off.
- `network_retries` now retries connection failures (it was never read);
  requests that may already have reached the server are never retried.
- Tool arguments sent as JSON objects (Ollama and older vLLM) are accepted.

### Permissions and safety
- "Always" on a shell approval card allowed every shell command for the rest
  of the session. It now adds per-segment rules: `program subcommand` with
  flags becomes a prefix rule (`cargo test ...`), anything else is allowed
  only verbatim (`rm -rf build` never grows into `rm -rf build ~`).
- Shell allow-rules never match commands containing `$(`, backticks,
  redirections, backslash escapes or `$'..'`.
- The permission check, the approval card, the diff preview and the tool now
  read arguments through one resolver, so the command that is approved is the
  command that runs (a stray `"arguments"` wrapper could make them differ).
- The approval card always shows the command, path or MCP arguments being
  approved — wrapped, never clipped, with whitespace padding made visible and
  control characters neutralised.
- MCP read-only annotations (`readOnlyHint`) and the `read_only` /
  `read_only_tools` settings in `.mcp.json` now actually skip approval cards.
- Interrupted or timed-out shell commands are killed with their whole process
  tree; a backgrounded child no longer hangs the tool call; stdin is closed.
- Subagents are cancelled together with the turn that spawned them.

### MCP
- Server-initiated requests (`roots/list`, `ping`, ...) are answered and can no
  longer be mistaken for replies to our own requests.
- The client no longer advertises capabilities it does not implement.
- `tools/list` pagination, invalid-argument errors, and a real "Error"/"Stopped"
  status instead of showing broken servers as "Disabled".
- Timed-out or cancelled requests are forgotten and the server is sent
  `notifications/cancelled`.

### TUI
- Esc / Ctrl+C interrupt cooperatively (3 s hard-abort fallback) instead of
  killing the turn and discarding its history. Ctrl+C on an approval or
  question card now denies and interrupts instead of quitting the app.
- Settings open on the live session state and only persist defaults you change
  there (the view no longer writes the config file itself); "Run Tool Test"
  runs a real tool-calling probe (it always said "passed").
- `/compact` keeps the current turn intact and folds the summary into the single
  system message (it could orphan tool results or stack system messages).
- `--resume` no longer stacks an old system prompt, keeps compaction summaries
  from older sessions, and reports a missing session.
- The context gauge counts the real tool schemas and no longer double-counts memory.
- Late recaps no longer derail the live turn's rendering.
- `/effort default` uses the server's default preset; unknown efforts are rejected.
- `/skills` exists (it was advertised); skills resolve from project and global dirs.
- Removed settings that were never wired to anything (theme, accordion, approval
  policy, git diff preview, smart commit) and tips that advertised missing features.
- Errors render as error lines instead of fake user messages; update failures
  are reported instead of swallowed.

### Updater and releases
- Downloads are verified against the release `SHA256SUMS`.
- A Windows `.zip` (or any package) is never written in place of the executable.
- The updater picks the newest release by version, never downgrades a beta in
  the background, never falls back to another platform's archive, and keeps
  updating when you work inside other Rust projects.
- Releases publish to rolling `beta` / `stable` channels with checksums and a raw
  Windows executable.
- CI was red on Windows (bash-only MCP tests); fixed.
