# Can your model drive tools?

A model that describes a tool call instead of making one leaves the
transcript busy and the disk untouched, and nothing says so. FlashAgent has a
check for it:

```bash
flashagent --tool-test               # the model you have configured
flashagent --tool-test --all-models  # every chat model your server lists
```

The second form prints the markdown table below. Set
`FLASHAGENT_TOOL_TEST_JSON=<path>` to also write the raw per-scenario results,
so a published table can be checked instead of believed.

## What is measured

Eight small scenarios, each one something the agent loop does on real work.
A model that fails one fails a real task the same way, later.

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

Sampling is the same for every model: temperature 0, reasoning off, 1024
output tokens, one attempt per scenario. The fixtures leave nothing open to
interpretation: an earlier version gave the model a tool result that began
with a line number, two models reported that number, and the fixture was
fixed.

A call written as text markup (`<tool_call>`, `[TOOL_CALLS]`, bare JSON)
counts as a pass, because FlashAgent's scanner recovers those and the loop
runs. The report says when that happened: it is slower and more fragile than a
native call, and a model that needs it will break on a backend without the
recovery.

The model is tested as the agent runs it, no more and no less:

- **The same rules.** Every scenario's system prompt ends with the rules the
  agent gives a model for calling tools (one step after another, independent
  calls together, a failed call corrected rather than apologised for).
- **The same repairs.** A call is judged after the repairs the loop makes
  before it runs one: `"20"` where the schema asks for a number becomes 20, and
  a lone argument under the wrong name (`status` where `code` is required and
  missing) takes the required name.
- **The same reminders.** Where the loop asks a model once more, the test does
  too, once: when a failed call is answered in prose (*recovers from a failed
  call*), and when the model stops before a call the request names (*moves on
  to the next tool*: "...then report it with report_status"). A pass that
  needed it is marked **after a nudge**: the work gets done, one round trip
  later.
- **The server's failures are not the model's.** A scenario the server refused
  (a rate limit, a 5xx, a dropped connection) is marked `--` and *not tested*,
  and the verdict says the run is *incomplete*.

## Results

Raw per-scenario output: [`docs/tool-calling/results-2026-09-12.json`](tool-calling/results-2026-09-12.json).

Measured on one machine (LM Studio, CUDA, one model loaded at a time). Time
is the total for all eight scenarios, prompt processing included, so it is an
order of magnitude, not a throughput figure.

| Model | Size | Score | Calls | Time | Failed |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `qwen3.6-35b-a3b-mtp@iq3_xxs` | 35B-A3B, IQ3_XXS | **8/8** | native | 48s | — |
| `gemma-4-e4b-it@iq4_xs` | 7.5B, IQ4_XS | 7/8 | native | 21s | recovers from a failed call |
| `gemma4-overlooked.thinker.uncensored-e2b` | 4.6B, GGUF | 7/8 | native | 8s | recovers from a failed call |
| `gemma-4-e2b-it-qat@q4_k_xl` | 4.6B, Q4_K_XL | 6/8 | native | 9s | recovers from a failed call; two files in one turn |

### What this run showed

**Every model here makes native tool calls.** None needed the parser for
calls written as text.

**Three of four stall on the first tool error.** Given a tool result that says
`error: no such file: src/confg.rs (did you mean src/config.rs?)`, they
explain the problem in prose instead of calling the tool again with the
corrected path. Only the 35B model retried. Tools fail all the time in real
work (a wrong path, a build error, a missing dependency), and a model that
answers each failure with a paragraph leaves the next step to you.

**The smallest model reads one file at a time.** Asked for two, it read one
and waited; the others asked for both. That costs turns, not correctness.

**The 35B passed everything and took five times as long** as the 4.6B for the
same eight scenarios.

## Reading the verdict

- **drives tools reliably**: full marks.
- **usable, with one rough edge**: one short. Fine for one-step work; watch it
  on longer chains.
- **unreliable**: half or better, but it needs correcting every turn.
- **mostly fails / cannot drive tools**: use it for chat, not as an agent.

The score says nothing about how well the model writes code, only whether it
can do the work through tools.
