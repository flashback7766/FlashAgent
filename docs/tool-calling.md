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
  calls together, a failed call corrected rather than explained).
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

### Small local models, before and after b450

b450 gave the model the agent's rules for calling tools, described every tool
parameter, and taught the loop the repairs and reminders listed above. Seven
small models on Ollama 0.34.4 (default tags), CPU only, one model loaded at a
time, before and after. The before column is the b425 test, which had none of
them.

| Model | Size | b425 | b450 | Still fails |
| :--- | :--- | :--- | :--- | :--- |
| `gemma4:e2b` | 4.6B | 7/8 | **8/8** | — (recovers from a failed call after a nudge) |
| `qwen3:1.7b` | 1.7B | 5/8 | **8/8** | — (moves on to the next tool after a nudge) |
| `qwen2.5:1.5b` | 1.5B | 5/8 | **8/8** | — (moves on to the next tool after a nudge) |
| `llama3.2:3b` | 3B | 4/8 | 6/8 | answers plainly when no tool is needed; keeps content exact |
| `qwen3:0.6b` | 0.6B | 5/8 | 6/8 | keeps content exact; recovers from a failed call |
| `llama3.2:1b` | 1B | 1/8 | 2/8 | six scenarios |
| `gemma3:1b` | 1B | 2/8 | 2/8 | no native tool calls on Ollama; six scenarios |

What moved them:

- **A failed call corrected, not explained.** Before, six of the seven
  answered `error: no such file ... (did you mean src/config.rs?)` with prose.
  Now qwen3 1.7B, qwen2.5 1.5B and llama3.2 3B retry with the corrected path
  on their own, and gemma4 e2b once reminded.
- **The second step.** The qwen models read the file and then told the user
  the number instead of reporting it with the tool the request named. All
  three report it once reminded of that call. llama3.2 3B reports it under
  the wrong argument name (`status`), which the loop now repairs.

What did not move: llama3.2 3B still calls a tool on a question that needs
none, and it and qwen3 0.6B drop the quotes or line breaks from file content.
The 1B models remain chat models. llama3.2 3B's b425 score counts two
scenarios that ran out of time while the model loaded; the test now loads a
local model before it times anything.

The rules were chosen by measurement, and small models are brittle about them:
a six-line version made llama3.2 3B describe a tool result instead of
answering from it, and "read the error and make a corrected call" left
qwen3 1.7B explaining the error where "do not explain the error: make a
corrected call" did not. Times are left out: on this CPU they measure the
machine, not the model.

Raw per-scenario output: [before](tool-calling/results-2026-09-26-b425.json),
[after](tool-calling/results-2026-09-26.json).

### Larger models on a GPU, b425 conditions

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
