# Can your model drive tools?

An agent is only as good as the model's willingness to call a tool instead of
describing one. That failure is quiet — the transcript looks busy while
nothing happens on disk — so FlashAgent ships the check rather than leaving
you to discover it an hour in.

```bash
flashagent --tool-test               # the model you have configured
flashagent --tool-test --all-models  # every chat model your server lists
```

The second form prints the markdown table below. Set
`FLASHAGENT_TOOL_TEST_JSON=<path>` to also write the raw per-scenario results,
so a published table can be checked instead of believed.

## What is measured

Eight scenarios, each one a thing the agent loop does on real work. They are
deliberately small: a model that fails them will fail a real task in the same
way, only later and more expensively.

| Scenario | The model is asked to | Why a failure hurts |
| :--- | :--- | :--- |
| **makes a tool call at all** | call `report_status` with a given code | without this the agent can only talk about doing the work |
| **gets the arguments right** | call `read_lines` with a path *and* a line count | wrong arguments edit the wrong file or run the wrong command |
| **answers plainly when no tool is needed** | answer a general question with tools advertised | a model that always calls something loops instead of replying |
| **uses a tool result** | answer from a tool result already in the history | otherwise every turn repeats the same call until a budget ends it |
| **moves on to the next tool** | after one result, call a *different* tool | multi-step work is the whole point of an agent |
| **keeps content exact through JSON** | write a file containing quotes and newlines | content mangled in transit corrupts the file being written |
| **recovers from a failed tool call** | after `error: no such file …`, retry with the corrected path | tools fail constantly; a model that cannot adapt stalls on the first one |
| **asks for two files in one turn** | read two files with two calls in one turn | one call per turn turns a ten-file job into ten round trips |

Sampling is fixed for every model: temperature 0, reasoning off, 1024 output
tokens, one attempt per scenario, no retries. Scenario fixtures contain no
ambiguity worth scoring — an earlier revision fed the model a tool result that
began with a line number, and two models sensibly reported the line number.
That was the harness being wrong, not the model.

A call written as text markup (`<tool_call>`, `[TOOL_CALLS]`, bare JSON)
counts as a pass, because FlashAgent's scanner recovers those and the loop
runs. The report says when that happened: it is slower and more fragile than a
native call, and a model that needs it will break on a backend without the
recovery.

## Results

Raw per-scenario output: [`docs/tool-calling/results-2026-09-12.json`](tool-calling/results-2026-09-12.json).

Measured on one machine (LM Studio, CUDA, one model resident at a time,
loaded and unloaded between runs). Time is the total for all five scenarios —
it includes prompt processing, so treat it as an order of magnitude, not a
benchmark of throughput.

| Model | Size | Score | Calls | Time | Failed |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `qwen3.6-35b-a3b-mtp@iq3_xxs` | 35B-A3B, IQ3_XXS | **8/8** | native | 48s | — |
| `gemma-4-e4b-it@iq4_xs` | 7.5B, IQ4_XS | 7/8 | native | 21s | recovers from a failed call |
| `gemma4-overlooked.thinker.uncensored-e2b` | 4.6B, GGUF | 7/8 | native | 8s | recovers from a failed call |
| `gemma-4-e2b-it-qat@q4_k_xl` | 4.6B, Q4_K_XL | 6/8 | native | 9s | recovers from a failed call; two files in one turn |

### What this run showed

**Every model here emits native tool calls.** None needed the text-markup
recovery path. That is worth knowing: the recovery parser exists for models
that do not, and on this sample it was never the thing standing between the
agent and the work.

**Three of four stall on the first tool error.** Given a tool result that says
`error: no such file: src/confg.rs (did you mean src/config.rs?)`, they
explain the problem in prose instead of calling the tool again with the
corrected path. Only the 35B model retried. This is the single most useful
number here: tools fail constantly in real work — a wrong path, a build error,
a missing dependency — and a model that turns each one into a paragraph makes
you the one driving.

**Batching two calls into one turn is rare at 4B.** The smallest model read one
file and waited; the others asked for both. It costs turns, not correctness.

**Size buys reliability, and you pay in seconds.** The 35B passed everything and
took five times as long as the 4.6B for the same eight scenarios.

## Reading the verdict

- **drives tools reliably** — full marks. Hand it a task.
- **usable, with one rough edge** — one short. Fine for one-step work; watch it
  on longer chains.
- **unreliable** — half or better, but you will be correcting it every turn.
- **mostly fails / cannot drive tools** — use it for chat, not for an agent.

A score here says nothing about how good the model is at writing code. It
says whether FlashAgent can hand it a tool and get the work done.
