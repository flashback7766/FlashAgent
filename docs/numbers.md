# Numbers

What the terminal app costs, measured. Each figure comes with the command
that produces it, so it can be checked instead of believed.

Measured 2026-09-22 on b328 code: AMD Ryzen 7 7735U, 32 GB RAM, Linux 7.2,
rustc 1.98.1, release build. Your own figures will differ with the machine.
What matters is how they grow.

## Size and start

| What | Figure |
|---|---|
| Binary, stripped (what releases ship) | 16.1 MB |
| From launch to a usable prompt | 41 ms (median of 5) |
| Memory when idle, welcome card on screen | 10 MB |

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
Studio with Gemma 4 E2B (Q4_K_M, 64k context) on the laptop above:

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

| What | Figure |
|---|---|
| `--resume` of a 2,000-turn session, to its last answer on screen | 81 ms |
| Memory with those 2,000 turns loaded | 21 MB |

Drawing the transcript (one turn = a question, a thought, a tool call and a
few lines of Markdown):

| Turns | First frame | Each frame while the model streams |
|---:|---:|---:|
| 100 | 0.6 ms | 0.03 ms |
| 1,000 | 6.6 ms | 0.37 ms |
| 10,000 | 64 ms | 3.4 ms |

```bash
cargo test --release -p flashagent-tui --test numbers -- --ignored --nocapture
```

The first frame renders everything once. After that, rows that can no longer
change are kept and shared between frames. Only the turn in progress is
rendered again. Before b328 each frame copied every kept row, and a streaming
frame at 10,000 turns cost 17.6 ms. The remaining cost still grows with
length, because of a few linear scans for the last question. At 10,000 turns
that is a fifth of a 60 Hz frame.

## Tests

Counted 2026-09-24.

| What | Count |
|---|---:|
| Tests in the workspace (`cargo test --workspace`) | 1,077 |
| Of those, the real binary in a pseudo-terminal (`crates/tui/tests/scenarios.rs`) | 91 |
| Every built-in tool called for real (`crates/tools/tests/every_tool.rs`) | 14 |
| Ignored: diagnostics that print screens, measurements, tests that need the internet | 11 |

The tests that need the internet run every night in CI
(`.github/workflows/web-tools.yml`). A release tag builds nothing until the
whole suite has passed on Linux, Windows and macOS.

## Tool calling

How reliably each local model calls tools is its own benchmark, with its own
page: [tool-calling.md](tool-calling.md).
