# Numbers

What the terminal app costs, measured. Each figure comes with the command
that produces it, so it can be checked instead of believed.

Measured 2026-10-22 on the b600 tree: AMD Ryzen 7 7735U, 29 GB RAM, Linux
7.2.9, rustc 1.99.0, release build. The figures of 2026-10-20 and of
2026-09-22 (b328) are kept beside them, so the trend is visible instead of
guessed. Your own figures will differ with the machine. What matters is how
they grow.

## Size and start

| What | 2026-10-22 | 2026-10-20 | 2026-09-22 |
|---|---:|---:|---:|
| Binary, stripped (what releases ship) | 19.0 MB (19,952,312 bytes) | 18.4 MB (19,298,736 bytes) | 16.1 MB |
| From launch to a usable prompt | 121 ms (median of 5) | 162 ms | 41 ms |
| Memory when idle, welcome card on screen | 57.5 MB | 35.2 MB | 10 MB |

```bash
cargo test --release -p flashagent-tui --test scenarios -- measure_startup_and_memory --ignored --nocapture
```

"Usable" means the prompt placeholder is drawn in a real pseudo-terminal
against a local mock model server, so the time includes reading the config
and asking the server which models it has.

**The memory grew by 22 MB, and this is where it went.** The app now counts
tokens with one counter for every number it shows, and the counter needs the
model's own tokenizer table. Loading it costs about 47 MB of resident
memory, measured on its own:

```
before: 3.4 MB · after: 50.6 MB · tokens: 5001
```

That table is the price of counting the way the model counts. The app is
still well under the 100–200 MB of Electron that [PHILOSOPHY.md](../PHILOSOPHY.md)
asks for, and the count it buys is the one that decides when to compact. A
cheaper estimate would mean a second number for the same text, which is what
the audit found and what this build removed.

## The first answer

A local server keeps what it last read and reuses the part a new request
begins with. The first message of a session has nothing to reuse unless the
app sends that part ahead (see `warm.rs`). Measured 2026-09-22 against LM
Studio with Gemma 4 E2B (Q4_K_M, 64k context) on the laptop above. This
table is the one that could not be measured again on 2026-10-20 or on
2026-10-22: there is no local model on the machine those days, and a cloud
model would be measured on someone else's tokens:

| What | Time to first token |
|---|---:|
| An 8.3k-token opening read from scratch | 33.8 s |
| The same opening once cached | 0.23 s |
| First message of a fresh session in the app, opening sent ahead (12k-token prompt, 99% cached) | 0.53 s |
| First step of a `/goal`, before the tool list was kept the same in and out of a goal | 8.55 s |
| The same, after (95% cached) | 1.45 s |

The first two rows come from requests sent to the server directly; the
third is the figure the app's token line showed.

## Long sessions

| What | 2026-10-22 | 2026-10-20 | 2026-09-22 |
|---|---:|---:|---:|
| `--resume` of a 2,000-turn session, to its last answer on screen | 161 ms | 242 ms | 81 ms |
| Memory with those 2,000 turns loaded | 65.9 MB | 43.8 MB | 21 MB |

Drawing the transcript (one turn = a question, a thought, a tool call and a
few lines of Markdown):

| Turns | First frame | Each frame while the model streams | 2026-10-20 (first / stream) | 2026-09-22 (first / stream) |
|---:|---:|---:|---:|---:|
| 100 | 1.2 ms | 0.01 ms | 1.2 ms / 0.01 ms | 0.6 ms / 0.03 ms |
| 1,000 | 9.9 ms | 0.08 ms | 13.5 ms / 0.17 ms | 6.6 ms / 0.37 ms |
| 10,000 | 112.7 ms | 1.25 ms | 151.1 ms / 1.78 ms | 64 ms / 3.4 ms |

```bash
cargo test --release -p flashagent-tui --test numbers -- --ignored --nocapture
```

The first frame renders everything once. After that, rows that can no longer
change are kept and shared between frames. Only the turn in progress is
rendered again. A new question used to throw the kept rows away and draw the
whole session again; since the cache survives a new question, only the new
turn is drawn. That is what the 2026-10-22 column shows: the frame after a
question, and the frame while the model writes, are both faster than they
were a fortnight ago, and the first frame is faster too.

The first frame is still about 1.5 times slower than it was in September,
because the width of a row is counted by graphemes and cells instead of by
characters -- that is what keeps wide characters, emoji and wrapped code from
breaking the layout (commit `b992f215`, 2026-09-24).

Numbers taken while the machine is also building something are not numbers:
the first measurement of 2026-10-20 came out three times too high because a
release build was running next to it. Before b328 each frame copied every
kept row, and a streaming frame at 10,000 turns cost 17.6 ms. The remaining
cost still grows with length, because of a few linear scans for the last
question. At 10,000 turns that is a fifth of a 60 Hz frame.

## Tests

Counted 2026-10-24 on the post-b605 tree, release build.

| What | 2026-10-24 | 2026-10-22 | 2026-10-20 | 2026-10-01 |
|---|---:|---:|---:|---:|
| Tests in the workspace (`cargo test --workspace`) | 1,673 | 1,658 | 1,485 | 1,267 |
| Of those, the real binary in a pseudo-terminal (`crates/tui/tests/scenarios.rs`) | 141 | 140 | 132 | 105 |
| Every built-in tool called for real (`crates/tools/tests/every_tool.rs`) | 20 | 20 | 24 | 15 |
| Built-in tools the model can call | 24 | 24 | 24 | 24 |
| Ignored: diagnostics that print screens, measurements, tests that need the internet | 12 | 12 | 12 | 11 |

```bash
cargo test --workspace
```

The count of 20 in the third row is what the test binary runs; the figure of
24 written down on 2026-10-20 counted something that is not there any more,
and the tools themselves are still 24.

The tests that need the internet run every night in CI
(`.github/workflows/web-tools.yml`). A release tag builds nothing until the
whole suite has passed on Linux, Windows and macOS.

## Tool calling

How reliably each local model calls tools is its own benchmark, with its own
page: [tool-calling.md](tool-calling.md).
