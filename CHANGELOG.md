# Changelog

## b399 — pasting an API key on Windows

- **The setup wizard takes a pasted API key on Windows.** The classic Windows console hands Ctrl+V to the program as a key instead of pasting, and the wizard ignored it, so a key could not be pasted at all. Ctrl+V (Ctrl+М on a Russian layout) and Shift+Insert now read the clipboard in the key, server address and model search fields.
- Spaces and line breaks copied along with a key are dropped, and the key's length is shown beside the mask, so you can see that the paste landed, and that it landed once.

## b398 — an audit of the whole program, and what it found

Five reviewers went through every part of FlashAgent looking for bugs; about ninety came back, and a second review checked the fixes. Update if you run b388: it has security and crash fixes.

- **Planning mode and "Always" rules can no longer be talked past.** A command could hide a second one behind a `#` comment, quotes inside a word (`-de''lete`), an abbreviated option (`git grep --op=rm`, `sort --comp=sh`), brace expansion (`{~,x}/.ssh`) or, on Windows without Git Bash, cmd.exe's own quoting. Rules now read a command the way the shell that runs it will. An "Always" for `git branch -a` no longer covers `git branch -D`, and the always-asked list knows other spellings: `git -C . push --force`, `push +main`, `--har`, `command rm -rf`.
- **An example is never run as a tool call.** A code fence that arrived in pieces could leave an example call outside it; a bare call split across pieces was missed.
- **Crashes fixed:** a web page with Cyrillic or CJK text next to `&amp;`, an API key typed on a Russian layout in the setup wizard, a server sending `Retry-After: -1`, a one-column window.
- **The setup wizard reaches every provider.** b388 added presets the wizard could not select, and OpenAI opened as a custom URL. Rows are picked with 1–9, 0 and c.
- **Compaction really frees the context.** The gauge counted the transcript still on screen, so every turn after the first compaction compacted again. Project instructions come back after compaction, and the summary never mistakes a tool's picture for your prompt.
- **Cloud providers:** a context overflow no longer strips sampling fields from every later request; discovery keeps the model you named instead of a longer similar one; OpenRouter is no longer taken for LM Studio.
- **Editing files:** `edit_file` keeps a CRLF file in CRLF and a BOM where it is, and counts a block found twice instead of calling it missing. A file named twice in one batch is shown as it will be written: the approval card used to hide the second part. `patch_file` puts a hunk after an insertion on the right line.
- **MCP on Windows:** `npx` servers start; an entry FlashAgent cannot run no longer drops the whole `.mcp.json`, and adding a server edits the file instead of replacing it.
- **Only text and colour reach the screen.** Escape codes in a command's output no longer clear the screen or move the cursor; tabs no longer leave old text showing; long highlighted code wraps without printing bits of colour codes and keeps its indent; wide characters no longer hide part of a command in the approval card; a card taller than the window keeps its title.
- **And more:** `/mode` no longer opens `/model`; Ctrl+E finds your editor and keeps line breaks; code pasted with tabs on Windows stays one paste; messages sent during a turn come back after Esc; the 16-colour theme stays readable on a white terminal; background tasks stop when FlashAgent exits; an update no longer leaves old copies of the program behind.

## b388 — compaction that keeps the thread, and any OpenAI-compatible provider

- **After compaction the model knows where it was.** The summary now follows a fixed handoff: your request and every instruction you gave, the files in play, errors and their fixes, your messages in order, what is pending and what was being done. It sees the tool calls with their arguments, not only their results.
- **Nothing said before compaction is lost.** The messages it removes from the context are kept word for word in `~/.flashagent/sessions/compactions/<session>.jsonl`, readable only by you, and the model is told where, so it can look an exact command or wording up.
- **A failed compaction changes nothing.** A summary cut short, one without the task or its state, or one that saves no space used to replace the conversation with one-line snippets; now the conversation stays as it was and you are told.
- **Any OpenAI-compatible provider works, not only the ones with a preset.** A field a server refuses by name (`top_k` on OpenAI, anything unknown on Mistral, `max_tokens` on newer OpenAI models) is left out from then on, instead of costing a refused request every turn. Rate limits and busy gateways are waited out when the server says how long. Several tool calls in one Gemini reply are no longer merged into one.
- **New presets:** OpenAI, DeepSeek, Mistral, Groq and Gemini, next to LM Studio, Ollama, vLLM, llama.cpp and OpenRouter.

## b383 — fixes to b378, one of them for your files' safety

An independent review of b378 found these; update if you run b378.

- **An edit's error no longer quotes a file outside the project.** b378 answered an edit that could not apply with the file's closest line, to save you an approval that would only fail. The check that the file was inside the project read the path as text, so `~/.aws/credentials` passed, and a prompt-injected model could read a line of it without asking. It now uses the same rule as the permissions (`~`, `..` and symlinks resolved), and any batch reaching outside the project goes to the approval card as before.
- **Git Bash runs commands exactly as written.** Every `\\` in a command reached bash as `\`, which silently changed `sed` patterns, `printf` output, JSON and Windows paths.
- **One odd line no longer garbles a whole command's output.** A single line in the console's legacy code page switched the decoding of everything after it; each read is now decoded on its own.
- **"I'll … once you confirm" is not a call left undone.** b378 asked the model to go ahead after sentences that waited on you, and after plain statements that began with "Теперь" or "Now". Only first-person announcements count now.
- **A newline inside a question or an option** no longer breaks the question card, and an edit that names the same file twice in one batch is checked on the result of its first part.

## b378 — one look everywhere, and a shell that works on Windows

A large release, so the number jumps by 25, as the scale in `packaging/bump.sh` counts it: a milestone plus a big fix.

- **Shell commands work on Windows.** They ran through cmd.exe, which knows none of the Unix commands models write, so `ls`, `cat` and `grep` failed wherever Git had not put its tools on PATH (the default). They now run in Git Bash when Git for Windows is installed; without it cmd.exe runs them, now with their quotes intact, and the model is told which shell it has. Output in the console's own code page, such as cmd's errors on a Russian system, reaches the model as text instead of mojibake, and a timeout or Esc stops the whole process tree.
- **A model that says "I'll do X" does X.** Small models often end a turn on the announcement and stop, so a `/goal` run could finish having changed nothing. When the last sentence of a reply says what the model is about to do and no call came, it is asked once to make that call.
- **An edit that misses says where it should have landed.** "old_string not found" now quotes the file's closest line and its number, an old_string copied from `read_file` with its line numbers still matches, and an edit that cannot apply goes back to the model straight away instead of asking you to approve something that would only fail. `edit_file` also tells the model that inserting a line means keeping its neighbour.
- **Approval diffs keep their indentation.** The preview dropped every leading space of the code being approved; it now shows it as it will be written, tabs as spaces and escape codes as text. A new file asks "Create this file?".
- **No "Compacting context failed" after a `/goal`.** A single long turn has nothing earlier to summarize, and automatic compaction no longer starts, and fails, on one.
- **Esc clears a steering draft before it stops the turn.** It used to stop the answer in progress even when you only meant to clear what you had typed; the hint under the prompt says which one Esc will do.
- **Approval cards ask a question.** "Run this command?" or "Change this file?" instead of "Confirm: edit_file", three buttons you can move between with the arrows (Always used to be unreachable), and the keys listed once, under the card. A diff preview no longer shows hunk headers or invented line numbers.
- **Every key hint reads the same.** The key, then what it does, a dot between: `Enter send · Esc cancel`, with the way out last. Four different styles are gone, along with bracketed notices, `...` next to `…`, and counts like "1 models".
- **Bad news looks like bad news.** A server that does not answer, a failed update and a model that cannot see an attached picture are announced in amber, not in the green of good news.
- **A first start without a server is honest.** The welcome card no longer invents a 128k context, the wizard says no server answered and how to go on instead of "auto-detected model: default", the tool check is skipped rather than failing every scenario and blaming the model, the prompt no longer shows a prefill estimate for a server that is not there, and a failed turn is explained once instead of twice. The folder question says what trusting a folder means.
- **Code in answers is coloured.** Keywords, strings, comments, numbers, types, calls and macros, for Rust, Python, JavaScript and TypeScript, Go, the C family, shell, PowerShell, SQL, Lua, Ruby, data files and diffs.
- **A light theme.** Settings → UI → Color theme → Light background; a terminal that reports a white background through `COLORFGBG` gets it without asking.
- **Questions from the model wrap.** A long question or option used to be cut at the card's edge; both now wrap, and the hint gives the real number range.
- **Tool lines tell the truth about groups.** "Ran 3 commands" when the first had failed now reads "Ran 3 commands · 1 failed", and a failed edit says so instead of showing its line counts in green. A running call's words sit where they will be when it finishes, and every tool line has its mark in the same column.
- **`/help` is two tables**, commands and keys, aligned and wrapped under themselves.
- **Smaller screens.** Settings, MCP and context panels fit a 44-column terminal, the exit card prints the resume command whole on a narrow one, and the welcome card drops whole hints instead of cutting one in half.
- **The Windows build starts on a fresh Windows.** It needed VCRUNTIME140.dll, which a clean install does not have; the C runtime is now built in.
- **The README starts with a quick start**, and the install scripts check each download against the published SHA256 sums, fail on a version that does not exist instead of installing the beta, and put the old binary back if anything goes wrong.

## b353 — the lines under the prompt, as they were

- **The tip, the prefill time and "Ready" are back.** b351 hid the tip while the model worked or the prompt had text, dropped the prefill time eight seconds after the first word, and took "Ready" off the status line. The lines under the prompt now behave as they did in b340. The new symbols stay: they replaced ones that showed as boxes.

## b352 — a steady download line, and answers typed straight into a question

- **The update line no longer flashes while it downloads.** Each step of the download put up a new notice, and a new notice fades in from grey, so the line blinked grey and green until the download finished. It now changes its numbers in place.
- **Type an answer of your own straight into a question card.** Letters used to be ignored until the last option, "Custom", was chosen; now the first letter opens the answer of your own. Digits still pick options. A path pasted into the card no longer has its digits picking options at random.

## b351 — every symbol drawn, every key heard, half the prompt

b350 was tagged with these changes and stopped by a test that tripped over a random tip; it was never published.

- **No more boxes instead of symbols on Windows.** The prompt arrow, the spinner, the context bar and a dozen other symbols were missing from Consolas, the font of a Windows console outside Windows Terminal, and showed as `?` in a box. Every symbol the UI draws is now one that Consolas and Cascadia Mono both have, and a test fails if one that is not creeps back in.
- **Shortcuts work with the Russian layout.** Ctrl+D arrived as Ctrl+в and did nothing; so did Ctrl+K, Ctrl+W, Ctrl+F and the rest that had no hand-written Russian twin. Any shortcut with Ctrl or Alt is now read by the key pressed, whatever the layout.
- **Pasted code keeps its braces.** With the Russian layout a Windows console delivers `{` and `}`, which that layout has no key for, as Alt codes, and they were dropped: `fn main() {` arrived as `fn main() `. They now arrive.
- **The system prompt is a third of its size.** 10.7K characters became 3.8K and the tool descriptions lost a 270-character note repeated on nearly every tool, so the first message of a session reads 2,700 tokens instead of 4,480. Measured on Gemma 4 E2B in LM Studio over 33 requests: the first answer of a cold session came in 32 s instead of 70 s, and the model did what was asked 32 times instead of 30.
- **Questions come as a card with answers to pick.** The model is told that every question to you goes through `ask_user` with two to six options, and the tool no longer offers to leave the options out. In the same run it asked that way 15 times out of 15, up from 14.
- **The model says what it is about to do.** Before a tool call it writes one short sentence ("I'll read tasks.py to see what it does"), in your language, then makes the call. In the same run it did so before 14 of 15 file and shell calls; before, 2 calls in 30 had a word in front.
- **Accept All reaches outside the project.** In Accept All the model may read and write files anywhere on the machine without a card, since the shell in that mode already could; it is also told the workspace is where it starts, not a wall. Manual and Accept Edits still ask, Planning and `/goal` still refuse.
- **Each line under the prompt shows what matters now.** The tip appears only while nothing else wants your eye: not while the model works, while you type, or while a card or menu is open, and its row stays blank so nothing moves. The prefill time shows for a few seconds after the first word, then gives way to the speed. The status line no longer says "Ready" next to an empty prompt.
- **The welcome card says each thing once.** The model was named twice and the mode was on the card and the status line; the card now shows the model, the context, the thinking effort and the rule files it loaded ("no rule files yet" instead of "0 active document(s)").
- **A thought is over when the answer starts.** It read "Thinking" until the whole turn ended; it now reads "Thought" once the answer below it has words. The live thinking line lines up with the finished ones instead of sitting one column left, and keeps its blank line under the question.
- **Code inside bold is code.** `**the `load` call**` showed its backticks; it is now bold with the code coloured, and the bold carries on after it.

## b340 — a screen that holds still and says less

- **The screen no longer flickers on Windows.** Every animation (the mascot breathing, the welcome card drawing itself in, a card's border pulsing) cleared the whole screen and drew it again, and between the two the screen was blank. Terminals that support synchronized output hid that; Windows' console does not, and Rust's standard output also hands it a frame in pieces of about 2 KB, drawn as they arrive. Now only the rows that changed are written, each over the old one, nothing is cleared first, and on Windows the frame goes to the console in one call. Watched through ConPTY, as under Windows Terminal, ten seconds on the welcome screen with animations on showed the screen half drawn 70 times; now it shows it 0 times, and sends 46 KB where it sent 350 KB. A streamed answer sends a fifth of the bytes. A test now fails if the screen is ever seen half drawn.
- **The setup wizard, the release notes and the directory question stopped flickering too.** They cleared and redrew the screen 20 to 25 times a second whether anything changed or not.
- **`/whatsnew` no longer drops the app onto the shell's screen.** It left the alternate screen on its way out, so the chat went on drawing over the shell's history. Events of a turn still running while it was open were thrown away; they now arrive after it closes.
- **Nothing on screen jumps.** A long tip used to take a second footer row while it typed itself out and give it back when it was erased, moving the whole conversation up and down every twelve seconds; the tip line now keeps one height at a given width. The welcome card holds its full height while it draws itself in, so the prompt under it no longer steps down the screen. A tip at rest no longer blinks a second cursor beside the real one.
- **Reading back while the model writes.** A transcript scrolled back with the wheel or PageUp used to creep upwards with every new line of the answer. It now stays where it was.
- **The terminal's cursor shows only where you type.** It used to blink on the bottom border of approval cards and open menus. The chosen colour theme now also applies to the setup wizard, the release notes and the directory question.
- **Esc never quits.** It closes a menu, then a card, then clears the prompt, and pressing it once too often used to close the app. Ctrl+D on an empty prompt quits, and so does Ctrl+C twice.
- **Ctrl+K finds any command** by its name, what it does, or its key: typing "f1" finds `/context`. A command that needs an argument, like `/goal`, is put in the prompt to be finished. Deleting to the end of the line moved from Ctrl+K to Alt+K.
- **Click a thought or a tool call to open or fold just that one.** F2 still does them all.
- **The conversation scrolls, the prompt stays.** PageUp, the wheel or Shift+↑ move the conversation; the prompt and the lines under it stay where they are, so a reply can be written while reading back. A line above the prompt says how far below the end the view is, and End or Esc returns.
- **Enter in the command popup runs what it highlights.** `/he` and Enter was sent as an unknown command while the popup showed `/help`. Aliases (`/expand`, `/retry`) and skills are listed once instead of twice, a one-letter search matches names rather than every description with that letter, and the popup stays away while an argument is typed. It no longer reads the skills folders from disk on every frame.
- **Less on screen while the model works.** The footer shows the spinner, the generation speed and the prefill speed; the token count, the speed graph, the cache percentage, the seconds and the face in the status line are gone, and so is "Generating response...", which the prompt already said. After a turn the status line is the mode, not a report. The tips no longer repeat the keys on the welcome card, and the key hints under the prompt appear once the card has scrolled away.
- **Sampling is for those who look for it.** F5 is gone, the setup wizard asks only for a preset, and the numbers are under Settings → LLM → Sampling (advanced) and `/sampling`.
- **The recap waits until you are done.** It is one more request to the model, and on a local server it held up the next question. It is now written after three minutes without a key pressed or a turn running.
- **The Windows taskbar shows a running turn.** In Windows Terminal the button fills while a turn runs, turns yellow while an approval card waits for you, and shows the download of an update, so a long `/goal` can be left in the background.
- **A burst of events is drawn once.** A fast stream or a held key drew a frame per event, which could leave the screen behind what it showed; frames now come at most one every 16 ms while events are queued.
- **The first message is answered as fast as the rest.** A local server reuses what it has already read, so inside a conversation the next reply starts at once, but the first one had to read the system prompt, the tool schemas and the memory block from scratch. On a laptop with Gemma 4 E2B in LM Studio that was 34 s before the first word. The app now sends that opening to the server while you are still typing, and again after `--resume` or a change of model or voice. The first message of a fresh session then showed a 97% cache hit and 0.48 s to the first token.
- **Starting a `/goal` no longer throws the cache away.** The plan tool was added to the tool list when a goal began, and the tool list sits near the top of what the server caches, so the first goal step read everything again: 8.5 s instead of 1.5 s on a fresh session, far more on a long one. The tool is now always offered and still refuses to work outside a goal.
- **A run stopped by a limit says so in words.** The chat showed the internal name, "— StepLimit —"; it now reads "— stopped: the step limit was reached —".
- **The README recordings were made again** on b330, with a throwaway home so no real session, memory or user name appears in them. `scripts/demo_env.py` holds what the five recording scripts share.

## b330 — a voice the model has, not a style it was told about

- **Styles work like a trait, not an instruction.** The style section used to be headed "the user's choice" and called itself a request, so replies began "as you asked, I'll keep it friendly". It now says who the assistant is, in the second person, with no word about a setting. Next to it goes one earlier exchange written in the chosen voice: a general question, and the answer this style and each characteristic would give. Models follow their own previous replies far more closely than any description, and they do not quote them. The example is sent right after the system prompt with each request and is never shown, saved, exported or compacted. The default style adds nothing.
- **The system prompt no longer quotes sample sentences.** Phrases such as "Hello! How can I help you with the project today?" and "Checking workspace structure..." were being repeated word for word by small models. The rules are now stated without examples to copy, and the model is told not to talk about its instructions. A voice that uses emoji no longer meets a rule against them.
- **Colour Theme can be selected in settings.** The UI tab drew seven rows but counted six, so ↓ on Animations jumped back to the top. The count now comes from the rows themselves, and a test walks every tab to its last row.

## b328 — a prompt you can edit, sessions that are never lost, and honest settings

- **Edit the prompt anywhere in it.** ←/→ move the cursor, Home/End go to the line and then the text, Ctrl or Alt with an arrow jumps by words, Delete, Ctrl+W, Ctrl+K and Ctrl+A work as in a shell, and all of them work with the Russian layout too. The cursor steps over what a person sees as one character, so an accented letter, a flag or a family emoji is one press of an arrow and one Backspace.
- **Prompts of several lines.** Alt+Enter, Ctrl+J or a `\` before Enter start a new line. A paste keeps its lines instead of joining them with spaces, so pasted code arrives as code. The box grows with the text up to a third of the screen, then scrolls around the cursor.
- **Ctrl+F finds an earlier prompt**, from this session or an earlier one: prompts are now kept in `~/.flashagent/prompt_history.jsonl` while sessions are saved. Enter takes the found prompt into the box to edit, it does not send it. Ctrl+R still regenerates the last answer.
- **`/resume` searches inside conversations.** Typing narrows the list by everything that was said, not only by the first prompt. When the match is in text the list does not show, the piece around it appears under the session.
- **`/export md` reads as the conversation.** No system prompt, no memory block wrapped around the first question, and tool results and thinking folded under `<details>`.
- **A conversation is never saved over another.** A `--resume` of a file that could not be read used to save the new conversation into that file, over what was left of the old one. Two instances started in the same second shared a session id, and the second one saved over the first. Neither can happen now.
- **Saves cannot be half written, and a failed save is shown.** Sessions and `config.json` are written beside the file and renamed over it. When a save fails, the app says so. On exit the conversation goes to the temporary folder and the message names it.
- **Pasting on Windows no longer sends the prompt line by line.** The Windows console delivers a paste as key presses, and every newline in it was an Enter. Keys that arrive together with a newline inside them are now read as one paste.
- **A long error wraps instead of being cut** at the window's edge, where the part that said what went wrong often was.
- **A paste goes to what has the keyboard.** An image path pasted as the answer to a model's question used to become an attachment for some later prompt.
- **Animations off means still.** The tips no longer type themselves, and the mascot no longer breathes or blinks. The welcome card no longer draws itself in, and the prompt arrow and the working face no longer move. The spinner still turns, because it is the sign that work is going on.
- **Token counters off are left out**, instead of reading "Tokens - 0 (0.0/s)" as if the model had stopped.
- **Narrow windows.** At 44 columns the permission mode was cut to "[Accept Edit". The context gauge now shrinks first, and the status line drops whole parts instead of ending on half a word. The MCP panel no longer lists its keys twice.
- **Long sessions stay fast.** A frame used to copy the whole transcript. At 10,000 turns a frame while the model streams went from 17.6 ms to 3.4 ms. Figures and the commands behind them: [docs/numbers.md](docs/numbers.md).
- **Releases are tested first.** A release tag now runs the full test suite on Linux, Windows and macOS before anything is built. `bump.sh` commits the version and then tags that commit, never moves an existing tag, and stops when the changelog has no section for the version.
- **Documents.** PHILOSOPHY now says what the owner decided: v1 is the terminal app, and the native UI is v2. ARCHITECTURE has diagrams of the crates, of one turn and of how a frame is drawn. New: [docs/numbers.md](docs/numbers.md) and a demo walkthrough, [docs/demo.md](docs/demo.md).
- **Removed.** Ten functions that nothing ran: five with no callers at all, and five that only their own tests called.

## b313 — paths with ~, working web search, colour themes and a readable transcript

- **A path written with `~` means the home folder.** A tool call has no shell behind it, so nothing expanded the tilde: `~/FlashAgent/src/main.rs` was looked for inside the project and reported missing. The permission layer made the same mistake in the other direction — because `~/.ssh/id_rsa` looked relative, it looked like an ordinary project file and nothing was asked before opening it. What a `~` means is now decided once and used by every tool and by the check that guards them.
- **Web search works again.** It had been answering "no results" to every query: the DuckDuckGo parser looked for markup the site no longer serves, and its last fallback searched for the start of a result from that result's own position. It now anchors on the class names both the full and the lite pages kept, unwraps DuckDuckGo's redirect and decodes numbered HTML entities. A page that is not a results page — a bot check, an unknown redesign — is an error saying so, instead of an empty answer. `BRAVE_API_KEY` in the environment switches to key-based search, and a rejected key falls back to the free one.
- **Colour themes.** Settings → Aesthetics → Colour Theme: Dark, Midnight, High contrast, Monochrome, and Terminal 16 colours, which hands the picture back to the palette the terminal itself uses and makes FlashAgent readable without 24-bit colour. A theme is applied to the finished frame, so every card is themed without knowing about it.
- **A transcript that reads as three things.** A blank row where the chat moves from the question to the work to the answer; `✻` marks thinking and `▸` marks a tool call, in the place a spinner takes while either runs; the answer stays at the left edge and the work sits in from it. What a running tool is doing is no longer written twice — it used to appear inside the composer as well, where it looked like text the user had typed.
- **Small terminals.** The welcome card was taller than the room under it below about 18 rows, so the terminal scrolled and its top was gone before it was seen. It now fits what is left after the composer, the hints, the tip and the status line, trying five shapes from two columns down to a single line. The tip takes a second row only in a window at least 20 rows tall.
- **Text that used to overflow the terminal.** A heading longer than the window was never wrapped, and a word with nowhere to break — a long URL, or writing without spaces such as Chinese — was printed whole. Both made the renderer's row count wrong, which is what erases the wrong rows on the next frame.
- **Reading files.** `read_file` reads a line at a time instead of loading the whole file to hand back twenty lines of it, and a file that is not text says so and says what to use instead, rather than reporting invalid UTF-8.
- **Settings that did nothing are gone.** `show_reasoning_accordion`, `approval_mode`, `git_diff_preview`, `git_smart_commit` and a per-preset `default_port` were written into `config.json` and read by nobody. Old config files still load; the keys are simply ignored.
- **Every built-in tool is now run for real by the tests**, by name and through the dispatcher, with the list checked against what the app offers so a tool cannot arrive unchecked. The two that need the internet run on a schedule instead of on every push, so a search engine changing its markup is a day's notice rather than a bug report months later.
- **Refactors.** The tool-card ladder (508 lines of `else if name == ...`) is a table of wording plus five methods for the tools that fold into the line above them; the welcome card takes one struct instead of eleven positional arguments; three unused MCP renderers and four wrapper functions removed.

## b300 — style & tone, memory summary, animations and a clean version scheme

- **Style and tone.** Settings → 7 Style picks how replies sound: a base style (Professional, Friendly, Candid, Quirky, Efficient, Cynical) plus Warm, Enthusiastic, Headers & Lists and Emoji at More / Default / Less. It changes tone only; tools and code are unaffected. Applies from the next message.
- **Memory summary.** `/memory summary` (or `s` on the memory screen) shows an overview of everything remembered, grouped by topic, with "Dive deeper" questions and an "Ask or update" field. Cached, marked stale when memories change, Ctrl+R rewrites it.
- **Animations.** Shimmer on thinking and running tools, cards that unfold, approval cards that breathe, a composer border that flashes by how a turn ended, a speed sparkline and a blinking caret. Settings → UI → Animations reduces them to spinners.
- **One version ordering everywhere.** Betas are `bN`, stable releases `vX.Y.Z+bN`. The updater, the channel card and what's new now agree; switching channel says plainly "Update: A → B" or "Downgrade: A → B". The beta channel also moves onto newer stable releases. See VERSIONING.md.
- **Safer keys.** An approval card that appears over open settings or the uninstall card now gets the keypress, not the screen under it. `/channel` asks before switching.
- **Fixes.** Long prompts stay visible while typing; the external editor (Ctrl+E) no longer loses keystrokes; Cyrillic command output is no longer garbled; the setup wizard opened mid-turn no longer hangs the app; a message sent as a turn ends is kept; `/clear` repaints; mistyped commands suggest the right one; `--url` applies to one run only; no flicker on repaint and no CPU use while idle.

## b287 — fast non-blocking startup and memory footprint optimization

- **Instant startup (no 10-second freeze).** Discovery of local and remote LLM endpoints now runs candidate probes concurrently (`/api/v1/models`, `/api/v0/models`, `/models`) with a short 1.5s probe timeout instead of blocking on 4 sequential requests.
- **Fast-path cached discovery.** Subsequent discovery polls (and server checks) reuse the working models endpoint directly, eliminating redundant endpoint scans.
- **Non-blocking TUI initialization.** Startup waits at most 500ms for server discovery before rendering the initial TUI frame. If the LLM server is remote, slow, or offline, FlashAgent renders immediately and completes discovery asynchronously in the background, forwarding results seamlessly without freezing the terminal.
- **Memory footprint cut by 55% (~9.7 MB RSS).** Tuned the Tokio multi-thread runtime worker pool (`worker_threads = 4`) and reduced thread stack allocation, cutting idle RSS from 23 MB down to ~9.7 MB and dropping thread count from 18+ to 6.
- **Connection timeout reduction.** Reduced `reqwest` connection timeout from 10s to 3s to prevent hung TCP SYN connections on unreachable or firewalled hosts.

## b286 — send screenshots without prompt text

- **Image-only message submission.** Pasting a screenshot from the clipboard (`Ctrl+V`) or dropping an image file on the terminal can now be submitted directly by pressing Enter without having to type any accompanying text.
- **Visual composer indicator.** When an image is attached and the prompt is empty, the input placeholder guides the user: `Press Enter to send image, or type a message...`.
- **Clean transcript formatting.** An image-only message renders cleanly as `❯ [screenshot 1920×1080]` in chat, without awkward leading spaces or empty lines.
- **Provider & protocol compliance.** Serializes image-only messages as content parts carrying only the `image_url` object with no empty text block, ensuring full compatibility with OpenAI, Anthropic, vLLM, and local vision models.
- **Session, rewind & export safety.** Gracefully handles image-only turns in session saving, restore, `/rewind`, `/compact`, `/export` (HTML/Markdown), and conversation recap.

## b285 — non-aborting context steering and unified status footer

- **Non-aborting context steering.** Typing a steering directive and pressing Enter while the agent is running no longer cuts off the streaming response mid-sentence or mid-tool-argument. Generation and tool execution run to clean completion, preventing partial assistant messages and preserving the entire KV prefix cache (cache hit ~99%).
- **Pinned steering in TUI.** While a response or tool is in progress, the steering directive is pinned at the bottom of the chat view (`❯ directive · steer queued`). Once the step finishes, the directive automatically unpins, joins the chat history as a regular user message, and the agent continues the dialogue seamlessly.
- **Unified status footer.** Turn telemetry and prompt cache hit metrics are consolidated into line 3 of the persistent footer, eliminating redundant duplicate status messages in the chat transcript.
- **First-turn cache display.** Suppresses `cache hit 0%` on the very first cold turn of a session when nothing was cached, keeping the footer clean.

## b284 — fix render panic and keep prefix cache across thinking changes

- **Multi-turn render panic.** Fixed a runtime panic (`slice index starts at X but ends at Y`)
  in the TUI renderer when viewing multi-turn sessions where earlier turns contained
  extracted `<think>` reasoning blocks.
- **Prefix KV cache preservation.** Turning thinking off (e.g. auto-effort on greetings)
  no longer appends a note to the user prompt. User messages remain identical across turns,
  preventing the server from discarding KV prefix cache when subsequent turns switch thinking on.

## b283 — how much of each prompt came from cache

Every finished answer now prints a status line underneath:
`status: 4.6s · 6.5K prompt · cache hit 98% · 71 out · 15.3 t/s · TTFT 0.41s · 3 calls`

- **Prompt cache visibility.** On local servers and cloud APIs that report
  prompt caching, you can see immediately how much of the conversation the
  server served from its cache versus what it had to evaluate from scratch.
  In multi-tool turns, prompt and cached tokens are summed across every call.
- **Provider support.** Reads cache hit metrics from OpenAI, OpenRouter,
  Gemini, xAI, vLLM (`prompt_tokens_details.cached_tokens`), DeepSeek
  (`prompt_cache_hit_tokens`), Anthropic-style APIs (`cache_read_input_tokens`),
  and llama.cpp (`timings.cache_n`).
- **Local LM Studio.** LM Studio's HTTP API does not return cache metrics, but
  its local server log records exact processed tokens for every call. When
  connected to LM Studio on localhost, FlashAgent reads the server log to
  compute the exact cache hit.
- **No guessing.** When a server reports no cache information and no server
  log is available, the status line shows `cache hit n/a` rather than an
  invented estimate.

## b282 — the prompt cache works again

This release changes nothing for you to use — it only fixes a bug, but one
you will feel: on a local server every reply after the first can start
many times sooner.

- **Every turn re-read the whole conversation.** On LM Studio and llama.cpp
  each new message was processed from the first token again — thousands of
  tokens and tens of seconds before the first word, with the cache reuse
  (f_keep) in the status line near zero. The cause was FlashAgent itself: it
  rewrote the start of the system prompt whenever thinking was switched on or
  off for a turn, and automatic effort switches it often (a greeting gets no
  thinking, a real task does). The server can only reuse its cache up to the
  first token that differs, and that token was the very first one.
- Now the system prompt is sent exactly the same every turn. When thinking is
  off for a reply, that is said in one line at the end of your newest message
  instead, which the server has not seen yet anyway. Measured against LM
  Studio with a 3 500-token prompt: flipping thinking between turns keeps
  f_keep at 0.998 and only the new message is processed.

## b281 — a proper way to uninstall

### Uninstalling

- **`flashagent --uninstall`** removes FlashAgent. It first shows what it
  will remove — every copy of the program, the PATH lines the installer
  added — then asks, part by part and with sizes, which data in
  `~/.flashagent` to delete. Caches and `/rewind` copies are ticked by
  default; settings, saved sessions, memory, MCP servers and skills are not.
  Nothing is touched until you confirm. `--uninstall -y` takes the defaults
  without asking, for scripts.
- **`/uninstall`** inside the app asks on a card first; yes saves the
  session, closes the app and runs the same uninstaller in the terminal.
- **`uninstall.sh` and `uninstall.ps1`** for when the binary is already
  broken or gone:
  `curl -fsSL https://raw.githubusercontent.com/flashback7766/FlashAgent/main/uninstall.sh | bash`
  or `irm https://raw.githubusercontent.com/flashback7766/FlashAgent/main/uninstall.ps1 | iex`.
  With a working binary they hand over to `flashagent --uninstall`.
- **PATH lines** are removed only where the installer wrote them (the lines
  under `# FlashAgent`), only once their folder holds nothing else, and a
  backup of each edited file is kept next to it
  (`.bashrc.flashagent-uninstall.bak`). A line you wrote yourself is never
  touched. The fish universal path and the Windows user PATH are cleaned the
  same way.
- **Package installs** (pacman, dpkg, xbps) are removed through the package
  manager — `sudo pacman -R flashagent-bin` and the like — so its database
  never lists files that are gone.
- A build from source refuses to uninstall itself: delete the checkout.
- The README has an Uninstall section.

### Faster next turn

- **Sending the next prompt stops the recap still being written.** The recap
  and suggestion for the previous turn used to keep generating after you had
  already sent another prompt — on a local server that made your new turn
  wait behind it. Now the request is dropped the moment the next turn starts
  (a prompt, `/goal`, or regenerate), and the server stops generating it.

## b280 — how FlashAgent is meant to behave, written down and enforced

A full audit against a written spec of expected behaviour. This release
changes how several everyday things work — read the list before updating.

### Quitting and starting

- **Esc twice to quit.** One Esc on an empty prompt used to close the app
  (or open a "Quit FlashAgent?" dialog). Now the first press says "Press Esc
  again to quit" and the second, within two seconds, quits. The session is
  saved either way. The first press also clears a pending suggestion, so
  quitting right after an answer is still two presses, not three. A late
  second press counts as a new first one. The welcome card and the hint line
  now say "Esc Esc quit".
- **FlashAgent starts in the mode you left it in.** The very first run starts
  in Accept Edits; after that, Shift+Tab, `/mode` and Settings all save the
  mode as the one to start in next time — Accept All included. The Accept
  All that `/goal` switches on for itself is never remembered; the mode it
  hands back is.

### Sessions

- **`flashagent --continue`** (or `-c`) opens the most recent session saved
  in the current folder.
- **`flashagent --resume`** (or `-r`) without an id opens a list of this
  folder's saved sessions — first prompt, how long ago, how many messages —
  to pick from. `--resume <id>` still opens one directly.
- **`/resume`** inside the app opens the same list. Picking a session saves
  the one that was open first, then swaps the conversation on screen, the
  model's history and where `/rewind` keeps its copies.
- The "Session Saved" card now mentions `--continue`.
- Fixed: `--resume ../../somewhere` could read a file outside the sessions
  folder and later save the session there. A session id is now a plain file
  name or it is refused.
- Fixed: two launches in the same second got the same session id, and the
  second session was saved over the first.

### Permissions

- **Files outside the project are asked about in every mode.** Reading or
  writing a file outside the folder FlashAgent was started in — `~/.bashrc`,
  `~/.ssh/id_rsa`, `../other-project` — now shows an approval card even in
  Accept Edits and Accept All, and is refused in Planning and during `/goal`,
  where nobody is there to answer. `..` and symlinks are resolved first, so
  a link inside the project that points at `~/.ssh` counts as outside. An
  "Always" given for a tool does not reach outside the project. This covers
  every file tool (read, write, edit, patch, list, outline, glob, git) and
  subagents too.
- **Dangerous shell commands during `/goal` are refused outright.** Force
  pushes, hard resets, recursive force deletes, `sudo`, `dd`, `mkfs` and the
  like used to stop a `/goal` run on an approval card nobody was watching.
  Now the command is refused and the model is told to find a safer way. In
  a hand-picked Accept All they still ask.
- **Planning runs commands that only read.** `ls`, `cat`, `head`, `grep`,
  `rg`, `find`, `wc`, `diff`, `git status/log/diff/show/blame`, the listing
  forms of `git branch/tag/remote` and similar run without asking. Anything
  that could write or run other code — redirections, `find -delete`/`-exec`,
  `sort -o`, `rg --pre`, `git -c`, builds such as `cargo check` — is still
  refused, and so is any argument that leaves the project (`/etc`, `~`, `..`,
  `$HOME`).

### Web tools

- **`web_fetch` and `web_search` are on by default** and run like a read, in
  every mode. They can be turned off in Settings → LLM & Reasoning → Web
  Tools. An old config's `free_search: false` no longer keeps them off.
- **Local addresses are asked about.** Fetching `localhost`, your local
  network, a router, or a cloud metadata address (`169.254.169.254`) shows a
  card in every mode and is refused in Planning and during `/goal` — a page on
  the internet can talk the model into such a fetch.
- A local address hidden some other way is always refused: a name that
  resolves to a local IP, an IP spelled oddly (`127.1`, `0x7f.0.0.1`), or a
  redirect from a public page. The address that was checked is the one
  connected to, and every redirect is checked before it is followed. Only
  `http` and `https` URLs are fetched.

### `/goal`

- **No limits by default.** A run used to stop at 250 steps or one hour.
  Now it runs until the task is done or you press Esc.
- **Limits live in Settings → Goal** (new tab 5): step limit, time limit,
  generated-token limit, each cycling through a few sizes and back to
  "Unlimited". The `--steps`, `--time` and `--tokens` flags are gone; typing
  one says where the limits went instead of starting a run.
- **The model may ask you something.** `ask_user` was blocked during
  `/goal`. Now a question shows its card with a countdown; if nobody answers
  within two minutes, the model picks the most reasonable option itself,
  carries on, and names that choice in its summary. Answers given before
  the timeout are kept.
- The report card says "cut short by a limit" and points to Settings.

### Updates

- **Updates install themselves in the background**, verified against the
  release checksums; the status line says "Updated to bNNN · restart
  FlashAgent to use it" once one is in. Background updates can be turned off
  in Settings → Updates.
- **Ctrl+U and `/update` join an update already under way** and show its
  progress, or check and install right away when nothing is running. The
  background updater and Ctrl+U never download at the same time.
- The "Silent Daily Notice" setting is gone: the background updater does
  the whole job.
- Fixed: an update to a binary in a folder you own was installed into
  `~/.local/bin` instead. The check opened the running executable for
  writing, which Linux refuses ("text file busy"); it now checks the folder,
  which is what replacing the file needs.
- Fixed: when the new binary could not be put in place, the update still
  reported success. It now says what failed, and on Windows the old binary is
  put back.

### Language

- **The interface is English only.** The Russian names for thinking stages
  ("Разбираю запрос") and the language setting behind them are gone. The
  model still answers in the language you write in.

### Documentation

- README, ARCHITECTURE.md, PHILOSOPHY.md, `/help` and the command list now
  describe all of the above: the permission table, sessions, `/goal`,
  updates, web tools and the keybindings.

## b275 — suggestions that stop asking you back

This release changes nothing for you to use — it only fixes a bug.

- The grey suggestion above the input box is meant to be your likely next
  message, but models kept filling it with the assistant's own question to
  you ("Specify the task you would like to work on first", "Tell me what
  you need help with"). The analyzer's prompt quoted a wrong example that
  models copied almost word for word, and it gave no way out after a plain
  greeting, where there is nothing concrete to suggest. The prompt now
  requires a subject that already came up and allows an empty suggestion,
  in which case none is shown. The filter now catches the pattern rather
  than individual sentences: in a message you send, "you" is the
  assistant, so "you" joined to a verb of wanting ("you need", "you would
  like") is always the assistant asking about your wants.

## b274 — thinking that forgot to say so

This release changes nothing for you to use — it only fixes a bug.

- A model with no native "reasoning" channel is asked to narrate its
  thinking as plain bold stage titles (`**Understanding the Request**`,
  `**Planning Implementation**`, ...) so it can still be collapsed like real
  reasoning. The code that collapses it, though, only recognized that
  narration when it opened with a literal lead-in line ("Thinking
  Process:", "Thinking:") — a lead-in the prompt never actually asked the
  model to write. A model that jumped straight to its first bold title
  (as instructed) had its whole multi-stage narration printed as the
  answer instead of collapsing under a "Thought: ..." line. It is now
  recognized by the stage titles themselves — at least two in a row,
  matched against the same short list of stage words FlashAgent's own
  prompt asks for — so a real answer that happens to open with its own
  heading (`**Summary**`) is left alone.

## b273 — /rewind asks first, and a floor under /goal

- `/rewind <n>` no longer takes turns back the moment you press Enter. The
  composer turns into a confirmation card — the same treatment the model,
  effort, and settings pickers get — listing every file that would change,
  each with its own `+added -removed` count, and flagging a file the turn
  created for removal. Nothing happens until "Yes, rewind" is confirmed;
  "No, cancel" (or Esc) leaves the files and the conversation exactly as
  they are.
- The plain "Rewound to before turn N · ..." line that used to print
  afterward is gone — the confirmation card already says what is about to
  happen, so a second, static echo of it after the fact was just noise.
- `/goal` now refuses a short list of shell patterns even though it normally
  runs everything without asking: `rm -rf`, `sudo`, `git push --force`,
  `git reset --hard`, `git clean -f`, `git branch -D`, `dd`, `mkfs`,
  `shutdown`/`reboot`. Hitting one of these pauses the run and asks you,
  the same as Manual mode would — even if you had earlier said "Always
  allow" to that exact command in a normal session.
- Every 10 completed steps, `/goal` checkpoints what it has written or
  edited so far as a git commit — only inside a project that is already a
  git repository, and only the files it actually touched. It sits next to
  the file snapshots `/rewind` already used, so a long run leaves commits
  to diff against along the way, not just "before it started" and "now".
- `/goal` can now keep a live plan: it calls a new `update_plan` tool
  (offered only during `/goal`) to lay out its steps and check them off as
  it goes, shown as a small checklist above the input that updates in
  place rather than piling up in the chat.

## b271 — a suggestion that only told you to make one

This release changes nothing for you to use — it only fixes a bug.

- The follow-up suggestion above the input box sometimes named a *category*
  instead of an actual prompt — "Укажи тему для анализа или задай конкретный
  вопрос по проекту" ("Specify a topic for analysis or ask a specific
  question about the project"). Pressing → sent that sentence itself to the
  assistant, which made no sense. It is the same failure already fixed once
  before for direct questions aimed at you ("Расскажи о своём проекте"); this
  time the model phrased it without the tell (no "своём/твоём") the filter
  was watching for. The filter now also catches this phrasing, in Russian
  and English.

## b270 — several files in one go

One thing you might notice: fewer steps when the work touches several files.

- The model can read several files in one call instead of one call per
  file, and change several files in one call. Each file of a read comes back
  under its own header, and a file that cannot be read says so without
  stopping the others.
- A change across several files is made whole or not at all: every edit is
  checked against its file first, and if one does not fit, no file is
  written and the model is told which edit failed. The approval card shows
  the diff of every file, and `/rewind` and the `/goal` report cover each of
  them.
- The line for such a call says what it covers: *Explored 2 files*, *Edited 2
  files +2 -2*, or the model's own header followed by the files, like *Read
  both notes · 2 files: one.txt, two.txt*.
- Unit tests and a scenario test cover it, each checked by breaking what it
  guards.

## b269 — answers that hit the length limit get finished

One thing you might notice: long answers no longer stop mid-sentence.

- When the server stops an answer at its output limit — cloud APIs often cap
  a reply at a few thousand tokens, and a local server does when its context
  fills up — FlashAgent asks the model to go on from where it stopped and
  writes the rest into the same message, up to three times. If the model
  starts by repeating its last words, the repeat is dropped before it reaches
  the screen.
- The request to go on is sent once and never kept: the next turn sees one
  whole answer.
- A tool call cut off by the limit is still never run, as before; the model
  is told to send it again.
- Four unit tests and a scenario test cover it, each checked by breaking what
  it guards.

## b268 — the argument names other agents taught your model

One thing you might notice: fewer failed tool calls from models that learned
another agent's way of calling tools.

- A model that writes a call the way another agent wants it — `filePath` or
  `file_path` for `path`, `oldString` or `TargetContent` for `old_string`,
  `cmd` for `command`, one edit given on its own instead of in a list — no
  longer gets "bad arguments" back and gives up or retries. The names are
  converted before anything reads the call, so the command on the approval
  card, the one the permission rules judge and the one that runs are still
  the same command.
- A call that already uses FlashAgent's own name for something keeps it, even
  if it also carries the other name. Tools from MCP servers keep the names
  their servers gave them.
- Seven unit tests and a scenario test cover it, each checked by breaking what
  it guards: no renaming at all, and a wrong name overwriting the right one.

## b267 — take a turn back

One new thing you can use: `/rewind`.

- `/rewind` lists the turns still in the conversation and how many files each
  one changed. `/rewind 2` takes back turn 2 and everything after it: every
  file FlashAgent wrote or edited in those turns returns to how it was
  before, files they created are removed, the conversation goes back to
  before turn 2, and that turn's prompt is put back in the input so you can
  change it and send it again. It works after `--resume` too, and an answer
  you regenerated is taken back to before its first attempt.
- What it cannot undo, it says so: changes a shell command made (FlashAgent
  only sees files through its own file tools), files over 8 MB (no copy is
  kept), and folders a turn created, which stay. A turn already folded into
  a `/compact` summary can no longer be picked.
- The copies live in `~/.flashagent/snapshots/`, next to the saved sessions.
- Nine unit tests and a scenario test cover it, each checked by breaking
  what it guards: no copy before a write, a turn not recorded, the
  conversation not cut back, the latest copy restored instead of the first.

## b266 — what the server says, not what the name suggests

Two things you might notice, one fix, and the rest is internal.

- FlashAgent no longer decides a model can reason because its name contains
  "qwen", "deepseek", "gemma-4" or "think". It goes by what the server says:
  LM Studio lists the reasoning settings of every model that has them, and a
  model the server says nothing about is left to its own default — thinking
  is not switched off for it, and no settings are invented for it. The
  thinking menu for such a model offers Auto and Off, the only two things
  that can honestly be offered.
- With llama.cpp's `llama-server`, the context window shown and used is the
  one the server runs with, read from its model list, instead of an assumed
  128k.
- The first turns after startup could go out carrying none of what the server
  had already reported about a model's reasoning. A b263 regression: sending
  turns and discovering the server were split into two backends so a turn
  never had to wait on its own first look, but the turn-sending one did not
  take over what the other had found until its own next look, up to 15
  seconds later. It now takes over immediately, so a "thinking off" you set,
  or a preset the server named, is already in force from the first message.
- The setup wizard tells LM Studio apart from other servers by what the
  server's own listing says, once it has answered, instead of guessing from
  the URL (`1234`, `lmstudio`) — the guess is still there for the moment
  before that. Mostly invisible; it matters for an LM Studio reachable
  through a hostname the guess would have missed, or a server that merely
  happened to sit on port 1234.
- Two more scenario tests, checked by breaking what they guard: one drives a
  turn right after startup and confirms the regression above cannot come
  back, the other confirms that a model the server lists without saying
  anything about its reasoning gets no reasoning fields on its turns at all —
  silence is not "off". Sixteen scenarios now run on Linux, Windows and
  macOS.

## b265 — the last of the scenario tests

Nothing changes for you in this release either. It is about stability, and
it finishes what b263 and b264 started: every path on the list for v1 is
now tested the way you use it.

- MCP: a tool marked read-only in `.mcp.json` runs without asking, and its
  result reaches the model; a tool that changes things asks first, and a
  refusal never reaches the MCP server.
- `/goal`: a run writes a file and runs a command without asking, the report
  lists both, and afterwards commands ask again as before. A run with
  `--steps 2` stops there, and the report says it is not finished.
- Each was checked by breaking what it guards. Fourteen scenarios now run on
  Linux, Windows and macOS with every change, and the scenario-test
  milestone on the road to v1 is done.

## b264 — nothing new to see, and that is the point

Nothing changes for you in this release. It is about stability: the parts
of FlashAgent you rely on most are now checked with every change, so a
beta that breaks them does not get released.

- Five more scenario tests — the real app in a terminal, against a stand-in
  model server. They cover allowing a command (it runs, and its output goes
  back to the model), denying one (it never runs, and the model is told),
  Esc during an answer (it stops, and what was already written is kept),
  `/compact` (the conversation so far becomes a summary the model is sent
  instead), and `--resume` (a saved conversation comes back, on screen and
  for the model).
- Each was checked by breaking what it guards — commands no longer asking,
  Esc no longer stopping, the summary dropped, a resumed conversation shown
  but not sent — and watching it fail. Ten scenarios now run on Linux,
  Windows and macOS.

## b263 — a quieter server, and the app tested the way you use it

Two things in this release. Only the first is something you might notice.

- FlashAgent asks your server which model is loaded far less often: every
  15 seconds instead of every 3, and never while the model is working — not
  during a turn, and not while the recap after it is still being written. On
  a laptop that runs its own model, that is one thing fewer competing with the
  answer. Against a test server, 50 idle seconds went from 36 of these
  requests to 10, and a turn with its recap from 14 to none. A model you
  switch in LM Studio still shows up, within 15 seconds.
- Apart from that, nothing changes for you: the rest is about stability, so
  that bugs like the last few do not reach a beta. FlashAgent now has
  scenario tests — the real app, started in a terminal, talking to a stand-in
  model server, checked by what is on the screen. The first five cover the
  setup wizard, quitting an empty session and one with a conversation, a turn
  that calls a tool, and the model list staying quiet while the model
  answers. Each was checked by breaking what it guards and watching it fail.

## b262 — square boxes, and a quit that does not ask for nothing

- Esc on a session where nothing was said quits straight away. It asked
  *Quit FlashAgent? The session is saved either way*, but an empty session is
  not saved and there is no `--resume` to come back to. A session with a
  conversation still asks.
- The approval card, the quit and channel dialogs, the question card and
  `/context` drew their top border one column short, so the right corner sat
  left of the side under it. Checked at 80, 110 and 140 columns.

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
  /tmp/.../audit_proj/main.rs` was all prefix and
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
