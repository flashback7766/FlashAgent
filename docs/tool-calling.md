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

Five scenarios, each one a thing the agent loop does on every turn. They are
deliberately small: a model that fails them will fail a real task in the same
way, only later and more expensively.

| Scenario | The model is asked to | Why a failure hurts |
| :--- | :--- | :--- |
| **makes a tool call at all** | call `report_status` with a given code | without this the agent can only talk about doing the work |
| **gets the arguments right** | call `read_lines` with a path *and* a line count | wrong arguments edit the wrong file or run the wrong command |
| **answers plainly when no tool is needed** | answer a general question with tools advertised | a model that always calls something loops instead of replying |
| **uses a tool result** | answer from a tool result already in the history | otherwise every turn repeats the same call until a budget ends it |
| **moves on to the next tool** | after one result, call a *different* tool | multi-step work is the whole point of an agent |

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

Measured on one machine (LM Studio, CUDA, one model resident at a time,
loaded and unloaded between runs). Time is the total for all five scenarios —
it includes prompt processing, so treat it as an order of magnitude, not a
benchmark of throughput.

<!-- results:start -->
<!-- results:end -->

## Reading the verdict

- **drives tools reliably** — 5/5. Hand it a task.
- **usable, with one rough edge** — 4/5. Fine for one-step work; watch it on
  longer chains.
- **unreliable** — 3/5. It will need supervision on every turn.
- **mostly fails / cannot drive tools** — use it for chat, not for an agent.

A score here says nothing about how good the model is at writing code. It
says whether FlashAgent can hand it a tool and get the work done.
