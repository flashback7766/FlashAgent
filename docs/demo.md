# Showing FlashAgent

A script for showing the app to someone in about fifteen minutes. The
order builds from what a first-time viewer understands at once (a chat
that reads files) to what makes the project different: the permission
layer, rewinding, `/goal`, and the tests that hold it together.

## Before

- A model server with a model that calls tools well. In LM Studio, a
  Qwen3 or Gemma 3 of 8B or more. [tool-calling.md](tool-calling.md) lists
  the ones measured. Load it before the audience arrives: the first load
  takes a minute.
- A small project to work in. A copy of this repository works, or a
  fresh folder with a short Python script that has a bug in it.
- A terminal of at least 100×30, with a font that has box-drawing
  characters (every modern monospace font does).
- `flashagent --tool-test` once, so the first thing on screen is not a
  surprise.

If there is no network or no model, the scenario tests in step 8 still
run. They bring their own scripted model.

## 1. Start (1 min)

```bash
cd ~/demo-project
flashagent
```

Show: the welcome card (model, context size, mode). Then make the terminal
window small and large again: the card and the footer fit themselves to
the window, down to 44×10.

## 2. A question that needs the files (2 min)

> What does this project do, and where would you start reading?

Show: tool calls as one line each (`▸ Reading …`), thinking marked `•` and
folded, the answer at the left edge. Press **F2** to unfold the thinking.

## 3. The permission layer (3 min)

Press **Shift+Tab** until the mode says **Manual**.

> Fix the bug in main.py.

Show: the approval card with the diff *before* anything is written. Choose
**Deny** once. The model is told not to retry and to ask instead. Then
allow it.

Then ask for something dangerous:

> Clean the repository with git reset --hard.

Even in **Accept All** that command still asks. It is on the blacklist.

## 4. Taking it back (1 min)

```
/rewind
```

Show: the card lists which files return to what, with the diff. After yes,
the files and the conversation are back to before that turn, and the
prompt is back in the input to edit.

## 5. Writing a prompt (2 min)

- Type a sentence, then move into its middle with **←** and fix a word.
  Show **Ctrl+W** deleting a word.
- **Alt+Enter** for a second line. Paste a block of code; it keeps its
  lines.
- **Ctrl+F** and a few letters find a prompt sent earlier, even in an
  earlier session.

## 6. `/goal` (3 min)

Set a step limit first: **Tab** → the Goal tab → Step Limit, say 20.

```
/goal add a --verbose flag to main.py and a test for it
```

Show: the budget burn-down in the status line, the live plan checklist,
and the report at the end: files touched, commands that failed, why it
stopped. The mode it switched to for the run is restored after.

## 7. Sessions (1 min)

Quit with **Ctrl+D**. The card on the way out gives the `--resume`
command. Then:

```bash
flashagent --resume
```

Type a word that only came up in the *answer* of an old session. The list
narrows to it and shows where the word was.

```
/export md
```

Open the file: the conversation as a document, with tool results folded
away.

## 8. What holds it together (2 min)

```bash
cargo test -p flashagent-tui --test scenarios
```

About a minute: 91 tests start the real binary in a pseudo-terminal,
type into it and read the screen, against a scripted model server. Then
open [numbers.md](numbers.md) for the figures: start-up, memory, and
frame time on a 10,000-turn session.

## If something goes wrong

- **The model answers but calls no tools.** It is a model that cannot.
  Switch with **F3**, or run `flashagent --tool-test --all-models` to see
  which of the loaded ones can.
- **Nothing happens after Enter.** The server is not up; the mascot on the
  welcome card goes grey while it does not answer. Start the server;
  FlashAgent finds it within a few seconds.
- **Colours look wrong.** Settings (**Tab**) → UI → Color theme →
  *Light background* on a white terminal, *Terminal 16 colors* where
  24-bit colour is missing, or *Monochrome*. A terminal that reports a
  light background through `COLORFGBG` gets the light theme by itself.
