# Numbers

What the terminal app costs, measured. Each figure comes with the command
that produces it, so it can be checked instead of believed.

Measured 2026-10-20 on 3f9dba3: AMD Ryzen 7 7735U, 29 GB RAM, Linux 7.2.9,
rustc 1.99.0, release build. The figures of 2026-09-22 (b328) are kept
below where they still stand, and the tables say which is which. Your own
figures will differ with the machine. What matters is how they grow.

## Size and start

| What | Figure | 2026-09-22 |
|---|---:|---:|
| Binary, stripped (what releases ship) | 18.4 MB (19,298,736 bytes) | 16.1 MB |
| From launch to a usable prompt | 162 ms (median of 5) | 41 ms |
| Memory when idle, welcome card on screen | 35.2 MB | 10 MB |

```bash
cargo test --release -p flashagent-tui --test scenarios -- measure_startup_and_memory --ignored --nocapture
```

"Usable" means the prompt placeholder is drawn in a real pseudo-terminal
against a local mock model server, so the time includes reading the config
and asking the server which models it has.

## The first answer

A local server keeps what it last read and reuses the part a new request
begins with. The first message of a session has nothing to reuse unless the
app sends that part ahead (see `warm.rs`). Measured 2026-09-22 against LM
Studio with Gemma 4 E2B (Q4_K_M, 64k context) on the laptop above. This
table is the one that could not be measured again on 2026-10-20: there is
no local model on the machine that day, and a cloud model would be measured
on someone else's tokens:

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

| What | Figure | 2026-09-22 |
|---|---:|---:|
| `--resume` of a 2,000-turn session, to its last answer on screen | 242 ms | 81 ms |
| Memory with those 2,000 turns loaded | 43.8 MB | 21 MB |

Drawing the transcript (one turn = a question, a thought, a tool call and a
few lines of Markdown):

| Turns | First frame | Each frame while the model streams | 2026-09-22 (first / stream) |
|---:|---:|---:|---:|
| 100 | 1.7 ms | 0.08 ms | 0.6 ms / 0.03 ms |
| 1,000 | 16.4 ms | 0.86 ms | 6.6 ms / 0.37 ms |
| 10,000 | 179.8 ms | 10.19 ms | 64 ms / 3.4 ms |

```bash
cargo test --release -p flashagent-tui --test numbers -- --ignored --nocapture
```

The first frame renders everything once. After that, rows that can no longer
change are kept and shared between frames. Only the turn in progress is
rendered again.

The 2026-09-22 column was measured on a quieter machine: the same commit
measured on this laptop in 2026-10-20 gives 9.4 ms for a 1,000-turn frame
against the 6.6 ms written down here. Measured side by side on the same
laptop, the code of 2026-10-20 is about 1.7 times slower than the code of
2026-09-22 (16.4 ms against 9.4 ms for 1,000 turns). The growth is in the
code, and it is tracked as its own task rather than hidden by writing the
older numbers back.

Numbers taken while the machine is also building something are not numbers:
the first measurement of 2026-10-20 came out three times too high because a
release build was running next to it. Before b328 each frame copied every kept row, and a streaming
frame at 10,000 turns cost 17.6 ms. The remaining cost still grows with
length, because of a few linear scans for the last question. At 10,000 turns
that is a fifth of a 60 Hz frame.

## Tests

Counted 2026-10-20 on 3f9dba3, release build.

| What | Count | 2026-10-01 |
|---|---:|---:|
| Tests in the workspace (`cargo test --workspace`) | 1,485 | 1,267 |
| Of those, the real binary in a pseudo-terminal (`crates/tui/tests/scenarios.rs`) | 132 | 105 |
| Every built-in tool called for real (`crates/tools/tests/every_tool.rs`) | 24 | 15 |
| Built-in tools the model can call | 24 | 24 |
| Ignored: diagnostics that print screens, measurements, tests that need the internet | 12 | 11 |

```bash
cargo test --workspace
```

The tests that need the internet run every night in CI
(`.github/workflows/web-tools.yml`). A release tag builds nothing until the
whole suite has passed on Linux, Windows and macOS.

## Tool calling

How reliably each local model calls tools is its own benchmark, with its own
page: [tool-calling.md](tool-calling.md).
