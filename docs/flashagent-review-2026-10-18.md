# FlashAgent: what each track did and how it was verified

Branch `model-contract` (HEAD `c9215ad`), everything pushed to `origin/model-contract`, no PR opened.
The release (version, changelog, tag) is not being prepared: by agreement it is one global
update after the tracks.

Checked at every step: `cargo test` over all crates, `cargo clippy --all-targets -- -D warnings`,
scenarios on a live terminal (`crates/tui/tests/scenarios.rs`, the binary in a pty) and a manual
run of the app in tmux. No models were loaded, `flashmodeld` was not touched.

A copy of this report is in the repository: `docs/flashagent-review-2026-10-18.md`.

---

## 1. Track 1 — stabilization

### 1.1 Static context budget — flat, 20k

The ladder of budgets by window size is gone: the prompt and the tool schemas cost the same for
any model, and everything left in the window goes to the conversation. A small window no longer
trims the tool set.

* `crates/core/src/budget.rs:28` — `const TOTALS: &[(usize, usize)] = &[(0, 20_000)];`
* Check: `the_flat_budget_applies_to_every_window`, `the_flat_budget_splits_what_the_prompt_leaves`,
  `the_registry_measures_against_the_flat_budget`.

### 1.2 base64 handed to an interpreter no longer passes

`echo <blob> | base64 -d | sh` hid the command from any check by words. A pipeline that decodes
text and hands it to an interpreter is now rejected.

* Commit `9437639`; the rule lives in the permissions layer and in the command check.
* Check: the scenarios in `crates/tui/tests/scenarios.rs` — a command of that kind
  does not reach the model.

### 1.3 The model does not downgrade itself

The auto-downgrade (a switch to a weaker model without asking) is removed: if the server or the
client cannot serve the request with the current model, that is shown, not fixed by changing
the model.

### 1.4 "No answer" does not disappear while the server is silent

The line under the prompt about there being no answer stays until the server speaks: before, it
vanished at the first change of mood.

* `crates/tui/src/main.rs:944` — the offline marker (the `offline_notice()` function, not a
  constant: the string is translated, and a check "by that string" must see the same thing).
* `crates/tui/src/events.rs:118`, `crates/tui/src/provider_switch.rs:89` — only a new answer
  from the server can take the line away.

### 1.5 Compaction does not lose what the model wrote

* A summary without the right shape, or a cut-off one, is no longer thrown away: the model's
  text is kept with a note — `crates/tui/src/compact.rs:295` (`missing_shape_note`), `:441`
  (`CUT_OFF_NOTE`), `:446` (`GROWTH_NOTE`).
* `ask_for_summary`: a cut-off answer is usable too — `crates/tui/src/compact.rs`
  (`usable = ended_cleanly || cut_off`).
* Auto-compaction still requires the window to shrink (`allow_growth=false`); the manual
  `/compact` does not, and says honestly that it did not shrink (`crates/tui/src/compact.rs:320`).
* `/compact` works with a single exchange: the exchange itself is summarized, the user's task
  stays (`crates/tui/src/submit.rs`, `crates/tui/src/compact.rs`).
* Compaction survives a restart: the session file is written right after it
  (`crates/tui/src/events.rs`), and the old format (the summary as a separate system message)
  is read on equal terms with the new one (`crates/tui/src/compact.rs:433`).

---

## 2. Track 2 — customization and UX

### 2.1 "Did you mean" (`fadb7aa`)

A typo in a command offers the nearest real one: `/compcat` → `/compact`.
A prefix is not a typo: "mode" is the start of "models", not a mistake.

* `crates/tui/src/autocomplete.rs:55` — `closest_match()`.
* Distance: two letters for short names, three for long ones (`allowed_distance`).

### 2.2 Export writes where it was told (`c4df211`)

`/export md /tmp/chat.md` writes to `/tmp/chat.md`; without a path — to the default file.

* `crates/tui/src/submit.rs:163` — `split_export_arg`, `:181` — `export_target`.

### 2.3 Tools can be turned off (`4e1cb75`)

A tool that is off is not offered to the model, and a call to it is rejected. Subagents see
the same set: the switch lives in two layers, not in one.

* `crates/core/src/config.rs:424` — `disabled_tools`.
* `crates/core/src/permissions.rs` — the permissions layer; `crates/tools/src/lib.rs:85` — the
  built-in tools (subagents have their own set, so the check is needed in both places).

### 2.4 History of notices (`7dcd632`)

`/notices` shows what the app said, so that it can be read again.

* `crates/tui/src/notice_log.rs:21` — `NoticeEntry`, `:37` — `NoticeLog`.

### 2.5 Safe mode (`f962338`)

On top of the permission mode, not instead of it: reading and editing inside the project are
allowed, everything else is not.

* `crates/tui/src/tools_view.rs:21` — `SAFE_MODE_TOOLS` (run_shell, web_search, web_fetch,
  spawn_agent, send_message, review_agent).
* `crates/core/src/config.rs:428` — `safe_mode`. In the footer next to the permission mode — `· Safe`.

### 2.6 Crash reports — only on request (`c31b791`)

Off by default: the report holds a backtrace and the paths of the machine.

* `crates/core/src/config.rs:432` — `crash_reports`; `crates/tui/src/main.rs:490` — `install_crash_hook`.

### 2.7 A project plan in the prompt (`afca86b`)

The model sees what lies where before it starts reading: folders up to three levels deep,
without noise.

* `crates/core/src/plan.rs:49` — `project_outline`; `crates/core/src/prompt.rs:194` — the
  `PROJECT PLAN` section in the prompt right after the environment block, limit 4000 characters.
* The module is named `plan` so it does not collide with the already exported `memory::outline`.

### 2.8 The interface in English and Russian (`3fdaaca`)

A person reads the interface, the model reads the prompt. Until now both were read in English,
and a Russian user was left to search for where "Escape" or "save and quit" was.

How it is built:

* `crates/tui/src/i18n.rs` — `Lang` (English/Russian), a table of (English, Russian) pairs and
  `tr()`: an untranslated phrase comes back in English. That is a gap, not a breakage, and the
  table grows one line at a time.
* The `interface_language` setting in the config (`crates/core/src/config.rs:435`, `en` by
  default) and the "Interface language" row in Settings: the switch applies **immediately** and
  survives a restart.
* The system prompt is not translated: the model still gets English.

Translated: the footer and the hints under the composer, `/help` (commands and keys), the tips
(TIPS_POOL), the welcome card and the placeholder, frequent notices, the names of the permission
modes, the command descriptions in the palette, the offline line.

### 2.8.1 The panels and the rest of the interface are translated (by the owner's decision)

Settings, the tool panel, MCP, memory, the context spending, background tasks, the selection
menus (headers, counters, empty states), the setup wizard, the folder trust screen, the effort
menu, the sampling panel, the descriptions of local servers and of what each tool does; then the
call cards in the conversation, the stages of the agent's work, the `/goal` report, the
explanations of server errors and the notices of the app. A number with its noun takes the
Russian plural, where English has only two forms: 1 retry, 2 retries, 5 retries.

What stayed in English and why: what the model reads (the compaction prompt, the memory
summary), the CLI texts for the developer (`--repeat`, `--scenario`, `--tool-test`) and the
sample questions in `/recap` — those are the words of the user, not of the interface. The tool
schemas for the model stay English — that is what the model reads.

### 2.9 Track 3 — compaction and quality

* **Map-reduce over chunks of 80% of the window.** A conversation that no longer fits in one
  window is no longer sent to the model whole: it is cut along message boundaries into chunks
  of 80% of what fits in a request (the window minus the prompt and the schemas), each chunk is
  compressed on its own, and the chunks are joined into one summary. The summary has the same
  shape as an ordinary one.
  `crates/tui/src/compact.rs`: `split_into_chunks`, `summarize_in_parts`, `reduce_request`.
* **The archive inside the session.** The early turns that the summary pushed out of the
  context used to lie in a separate file next to the session. Now the archive is the `archive`
  field in the session file: the session is one thing again, not a file with a companion. The
  sessions folder is closed with 0700 permissions.
* **A standing line about compaction.** The card is the summary itself and it scrolls up;
  after the compaction a line enters the conversation: what was compressed, how many tokens
  were freed and where the exact archive lies.
* **The xhigh/max levels.** The reasoning ladder had only off/low/medium/high; models with an
  effort ladder have two levels above high as well.
* **reasoning_tokens and cost_details.** Usage now carries thinking tokens
  (`completion_tokens_details.reasoning_tokens`) and what the provider counted provider by
  provider (`cost_details`): `/cost` now says that too — the difference from the sum is the
  markup of the gateway.

### 2.9a Track A — stabilization (done 2026-10-21)

Four findings from this review are closed, all with tests.

* **`edit_file` edits that overwrote each other.** Two edits in one call whose ranges overlap
  were applied "from the end" without a check: the second overwrote the first, and the model
  got "applied 2 edit(s)". Reproducible: `"x = foo()"` with the edits `foo`→`bar` and
  `foo()`→`qux()` gave `"x = bar()"` — the request was to replace `foo()` with `qux()`, and
  `bar()` stayed in the code. Now the overlap of the ranges is found before the edits are
  applied, and the call is rejected with a text that says which two edits, on which line and
  what to do. `crates/tools/src/fs_tools.rs`.
* **Compaction counted the free space with a double subtraction.** The space was counted as
  "the window minus the prompt and the schemas" and then minus "how much is already taken",
  and the taken part already included the prompt and the schemas. The same tokens were
  subtracted twice, the conversation — which is exactly what gets compressed — a third time.
  On a 128k window a 60k conversation was cut into parts (5-7 requests instead of one), and on
  a 64k window with a 40k conversation the space came out as zero, no split was made, and the
  whole transcript went in one request, which the server rejects for its length. Now the
  formula is one (`room_for` in `crates/tui/src/compact.rs`) and subtracts the fixed part
  exactly once.
* **A server from the project's `.mcp.json` started without a question.** The project config
  can run a program; the commands in it started right away when the folder was opened, before
  the first request of the person and even with the `-y` flag. A cloned repository could
  execute arbitrary code. Now the question names the server and the exact command and offers
  "Yes, and remember", "Yes, just this time" and "No, do not start it". The answer is
  remembered together with the folder and the command: a config that changed what it runs is
  a different program. The own global config (`~/.flashagent/mcp.json`) is the server of the
  owner, it goes without a question. In safe mode it asks every time and remembers nothing.
* **The test "the screen is never seen half-drawn" counted the echo of its own exit.**
  It failed on a loaded machine (and on a clean tree). The test kills the app at the end, and
  on the way out it returns the terminal, and the terminal shows the main screen — empty. The
  test took that for "the screen is drawn halfway". The watchdog now notices the exit from the
  alternate screen before it parses those bytes, and the threshold "more than 50 chunks read
  in 6 seconds" is replaced by waiting for the last word of the story on the screen: how many
  frames the app manages to draw is the business of the machine.

### 2.9b Track B — the speed and the quality of the prompt (done 2026-10-21)

Three findings are closed, all with tests.

* **The model and the reasoning mode moved to the end of the prompt.** The block stood second,
  and a change of model (F3) or of mode (F4) changed everything after it — the whole prompt
  went into a recount. A measurement from the repository: the reuse of the prefix was 2.7 % on
  a change of folder and 3.2 % on a change of mode. Now it is **100.0 %** on a change of mode
  and **99.8 %** on a change of model; on a local model that is about 12 seconds of waiting per
  switch that went nowhere. The measurement was turned from a print into a check.
  `crates/core/src/prompt.rs`.
* **The prompt no longer promises tools the model does not have.** The rules about the
  subagent and the rule "every question goes through `ask_user`" stood in the prompt always,
  even when the tool was off. The section about questions became conditional: without
  `ask_user` it does not say "ask through the tool" but "there is no way to ask, say it in one
  line and carry on" — otherwise the rule "never write a question as text" left the model with
  no way to ask at all. The flags come from the set of schemas the model actually sees, and the
  prompt is rebuilt when the set changes. `crates/core/src/prompt.rs`, `crates/tui/src/turns.rs`.
* **Memory that the model wrote no longer arrives as an instruction.**
  `MEMORY.md` is at once the text of the owner and the index of what the model remembered; the
  model writes the index itself. Everything was inserted as "standing instructions", and
  "always answer briefly" stood next to "ignore previous instructions". Now the file is split
  by the markers `<!-- flashagent:memory -->`: the text of the owner goes as an instruction,
  what the model wrote — as a separate document, marked as a record.
  `crates/core/src/memory.rs`.

Also: `PHILOSOPHY.md` required "the interface and the repository in English only", although the
interface has been bilingual since `3fdaaca`. The line was brought in line.

### 2.9c Track C — customization (done 2026-10-21)

* **One counter counts the tokens, and it counts by model.** Tokens were counted in three
  places by three different rules: the budget of the schemas — by the tokenizer, the context
  panel and the compaction — by dividing the number of bytes by four. The rule with the bytes
  was the worst: a Russian letter is two bytes, and the panel said the window was half empty
  when the model had read a third of it. The "four bytes per token" rule is gone everywhere,
  everything goes through `count_tokens(model, text)`. Whose tokenizer it is is a question too:
  for OpenAI models there is its own table, for all the others `cl100k_base` was taken — a
  foreign table, and against the real tokenizer of Qwen it was wrong by 12 % on average and by
  80 % in the worst case (measured on 39 real texts). Now `o200k_base` is taken: 6 % on
  average, 30 % in the worst. Further on — measurement: every server says how many tokens are
  in what it wrote, and the app has that text; together that is an exact measurement of the
  model, and it is free. The measured model is counted with an error of 2.4 % on average
  instead of 6.1 %. The measurements live in `~/.flashagent/tokens.json` and carry over into
  the next run. `crates/llm/src/tokenizer.rs`.
* **The buttons of the permission card are translated.** "Run this command?", "Allow",
  "Always allow", "Deny" went around the table of translations: the card spoke English in the
  middle of a Russian interface. `crates/tui/src/render.rs`.
* **A collector of untranslated strings.** It walks the sources of the interface and finds the
  phrases that reach the screen without a translation: not in the table, not from a `tr()`
  call, looking like text for a person. 32 phrases were found, twenty were translated (the
  trust screen, the server errors, the `/goal` report, the `/rewind` rollback, the `/mcp`
  help, the memory panel, export, the wizard, the sampling labels), twelve are in English on
  purpose — each with a reason in the list. The list keeps it from being forgotten: a test
  checks that every entry is still there and that none was left in the code without a reason.
  `crates/tui/src/untranslated.rs`.
* **`/sampling` got the response limit and the sampling seed.** "Max tokens" — how much the
  model may write in one answer (an empty field means "the server's own limit"); "Seed" —
  with the same seed and the same request the answer comes out the same. Both reach the
  server: `max_tokens` in the OpenAI-compatible request, in `generationConfig` for Gemini and
  in `options` for Ollama. `crates/tui/src/sampling.rs`.
* **Standing "Always" rules survive a restart, and `/rules` shows them.** A rule given by the
  "Always" button lived in memory until the end of the session: in the morning it had to be
  given again, though it was given on purpose. And there was nowhere to look. Now the rules
  live in the config, survive a restart and are shown on the `/rules` screen: a tool, a
  command prefix or a whole command; `d` removes one rule, `x` removes all of them.
  `crates/tui/src/rules_view.rs`.

### 2.10 Track 4 — a flexible `edit_file`

* **Editing by line numbers.** `start_line` and `end_line` — from one, the last line
  inclusive, exactly as the file was read. The text of the model takes the place of those
  lines entirely, with the right indentation and with the same line ending as the file.
* **Regular expression.** `regex: true` makes `old_string` a regex, and in `new_string` the
  match can be brought back with `$1` and `${name}`.
* **Several cursors.** All the edits of one call are located in the file as it was, and are
  applied from the end: the line number does not drift when another edit changes the file
  above. An edit that depends on the previous one is still applied in order.

`crates/tools/src/fs_tools.rs`: `plan_one_edit`, `plan_all_edits`, `line_span`.

### 2.11 `docs/numbers.md` recounted

See §3.

---

## 3. Numbers

Recounted 2026-10-20 on the code at `3f9dba3` (the two commits after it do not touch the
render), release build, on the same laptop (AMD Ryzen 7 7735U, 29 GB RAM, Linux 7.2.9,
rustc 1.99.0).

| What | Was (2026-09-22, b328) | Became (2026-10-20) |
|---|---:|---:|
| Binary, stripped | 16.1 MB | 18.4 MB (19 298 736 bytes) |
| To the first ready frame (median of 5) | 41 ms | 162 ms |
| Memory when idle | 10 MB | 35.4 MB |
| `--resume` of a 2 000-turn session, to its last answer | 81 ms | 243 ms |
| Memory with 2 000 turns | 21 MB | 43.6 MB |
| Session frame at 1 000 turns (first / while streaming) | 6.6 / 0.37 ms | 16.4 / 0.86 ms |
| Session frame at 10 000 turns (first / while streaming) | 64 / 3.4 ms | 179.8 / 10.19 ms |
| Tests in the workspace | 1 267 | 1 485 |
| Scenarios (the binary in a pty) | 105 | 132 |
| Built-in tools called for real | 24 | 24 |

The commands are the same as in the file:
`cargo test --release -p flashagent-tui --test scenarios --
measure_startup_and_memory --ignored --nocapture` and `cargo test --release -p flashagent-tui
--test numbers -- --ignored --nocapture --test-threads=1`.

The numbers of the "first answer" (LM Studio, Gemma 4 E2B) cannot be remeasured here: there is
no local model at hand, and there is no need to reach into the cloud with someone else's
tokens. That table is from 2026-09-22 and is marked as old.

A measurement taken while a release build runs next to it comes out three times higher than
the real one: the numbers were taken on a machine that was building nothing.

---

## 4. What was not done

* The CLI texts for the developer and the sample questions in `/recap` — in English on purpose
  (see §2.8.1).
* The release: version, changelog, tag — by agreement, one global update after the tracks.

## 5. Risks worth knowing about

* **The render of long sessions.** The culprit of the slowdown was found by bisecting the
  measurement: `b992f215` ("Rendering fixes from the audit", 2026-09-24) — after it the width
  of a row is counted by graphemes and cells (`unicode_segmentation` + `unicode_width`), not by
  characters. Wide characters, emoji and wrapped code no longer break the layout because of
  that, but every frame pays more for the width. Fixed (`f9c9f09`): the cache of drawn rows
  survives a new reply, and the whole conversation is no longer drawn again for every
  question. At 10 000 turns: the frame while streaming 10.19 ms → 1.78 ms, the frame after the
  question 180 ms → 8.4 ms. The first frame of a session (after the start or a change of the
  window size) stays full: 151 ms.
* **The three tests that were failing turned out to be stale expectations**, not regressions:
  `/compact` now summarizes a single exchange as well, and the `/help` list became longer than
  the window. The expectations were fixed (`3f9dba3`), and the build of
  `crates/tools/tests/web_live.rs` was repaired as well; it had not built since the tool switch
  appeared.
* **The glyph test had not run since the tool list appeared** and it found a "✔" — a
  character that Consolas and Cascadia Mono do not have (on Windows it showed as a square).
  The sign of the plan step (`√`) was taken instead, the test is green again (`8893908`).
* **The translation table lives in the code** (`crates/tui/src/i18n.rs`): that is a deliberate
  compromise (the phrases are finite and known), but a fix to a translation is a fix to the
  code and a rebuild. A phrase the table does not hold is shown in English — that is visible at
  once and is fixed with one line.

## 6. What this round did (commits)

| Commit | What |
|---|---|
| `3fdaaca` | The interface in English and Russian: the `i18n` module, the `interface_language` setting, the row in Settings, the translation of the footer, `/help`, the tips, the cards, the placeholder, the notices and the modes |
| `3f9dba3` | The stale expectations of two scenarios and a fix to the build of `web_live.rs` |
| `ace90be` | `docs/numbers.md` recounted from the facts, the old numbers left beside them |
| `a85835a` | The caption of the tip translated, its width counted by it |
| `8893908` | The sign in the tool list replaced with one that exists in both Windows consoles |
| `7ce4387` | The panels translated: Settings, tools, MCP, memory, context, tasks, the wizard, trust, effort, sampling |
| `c9215ad` | The descriptions of the tools translated; the layout of the context panel and the order in the MCP panel repaired |
| `f9c9f09` | Render: the cache of rows survives a new reply, the whole conversation is no longer drawn again |
| `70b4541` | The numbers after the render was repaired |
| `54a891d` | The call cards, the stages, the `/goal` report, the server errors and the notices translated further |
| `c29b59e`, `18efcbd` | Compaction: a conversation that does not fit the window is compressed in parts; the arguments split into structures |
| `f1bab7a` | The xhigh/max levels and the price the provider named |
| `db21fa5` | The archive of the compaction — in the session itself |
| `e66a62c` | A standing line about compaction in the conversation |
| `4b5d6d7` | A flexible `edit_file`: lines, regexes, several cursors |
| `8a8541d` | `edit_file` edits that overwrote each other are rejected |
| `7ae1b74` | Compaction: the free space is no longer counted with a double subtraction |
| `07414b6` | The "half the screen" test no longer counts the echo of its own exit |
| `fade935` | A server from the project's `.mcp.json` asks before it starts |
| `ffaceb5` | The model and the reasoning mode go to the end of the prompt, the cache survives the switch |
| `533961d` | The prompt no longer promises tools the model does not have |
| `199a24e` | Memory that the model wrote no longer arrives as an instruction |
| `333dc89` | One counter counts the tokens, and it counts by model, not by rule |
| `dc7e47a` | The buttons of the permission card translated, and the collector looks for untranslated strings |
| `fdb2f6f` | /sampling got the response limit and the sampling seed |
| `1b17a81` | The standing "Always" rules survive a restart, and /rules shows them |
| `456bfb4` | The profile of the tool set removed: the set is one anyway, and the switch is turning individual tools on and off |

## 7. How to check it yourself

```bash
cargo test --workspace                      # 1 485 tests
cargo clippy --all-targets -- -D warnings   # clean
cargo test -p flashagent-tui --test scenarios   # 132 scenarios on a live terminal
```

The Russian interface is switched on in Settings → General → "Interface language", or with a
line in `~/.flashagent/config.json`:

```json
{ "interface_language": "ru" }
```
