use super::*;
use flashagent_tui::i18n::{tr, tr_fill};

const HELP_COMMANDS: &[(&str, &str)] = &[
    ("/goal <task>", "an autonomous run; its limits are in Settings \u{2192} Goal"),
    ("/resume", "pick a saved session from this folder and continue it"),
    ("/settings, Tab", "settings"),
    ("/theme [name]", "preview a color theme; Enter saves, Esc cancels"),
    ("/model, /m, F3", "choose and switch the model"),
    ("/provider [name]", "switch to another saved provider, local or cloud; add or edit them"),
    ("/effort, /t, F4", "thinking effort (also Ctrl+T)"),
    ("/mode [plan|man|edits|all|yolo]", "permission mode (also Shift+Tab; YOLO needs Settings)"),
    ("/prompt [auto|compact|standard|full]", "how much system prompt a model gets; auto follows its window and backend"),
    ("/context, F1", "what fills the context window"),
    ("/usage", "session, saved-session and lifetime tokens and cost, plus provider limits"),
    ("/compact [focus]", "summarize older turns to free context"),
    ("/agents", "subagents: live transcript, message or stop one"),
    ("/memory [summary]", "what FlashAgent remembers; the summary groups it by topic"),
    ("/migrate-memory [run|verify|cleanup]", "convert the old memory layout to MEMORY.md + topic files, with a backup"),
    ("/verbose [all|last|off]", "show thoughts and tool calls (also F2, Alt+O, Ctrl+O)"),
    ("/regenerate, Ctrl+R", "answer the last prompt again from scratch"),
    ("/rewind [n]", "files and conversation go back to before turn n"),
    ("/diff, /commit <msg>", "the changed lines and untracked files; commit the staged changes"),
    ("/export [md|html|jsonl] [path]", "write the conversation to a file, where you say"),
    ("/editor, Ctrl+X Ctrl+E", "write the prompt in an external editor"),
    ("/tools", "what the model may call, and what it may not"),
    ("/notices", "what the app said, kept so it can be read again"),
    ("/rules", "the standing permissions, and a way to take them back"),
    ("/safe", "the safe mode: reading and editing inside the project, nothing else"),
    ("/mcp [list|market|test|add|reload]", "Model Context Protocol servers"),
    ("/tasks", "background commands: their output, and stopping one"),
    ("/sampling", "sampling parameters (advanced)"),
    ("/skill:<name>", "run a skill from .agents/skills/"),
    ("/update", "check for, download and install an update"),
    ("/channel <stable|beta>", "switch the release channel"),
    ("/clear", "clear the screen and scrollback"),
    ("/uninstall", "close FlashAgent and remove it; asks what data to delete"),
    ("/exit", "save the session and quit"),
];

const HELP_KEYS: &[(&str, &str)] = &[
    ("Enter", "send; while a turn runs, steer it, or run a /command (/compact, /clear, /export and /rewind wait for the turn to end)"),
    ("Alt+Enter, Shift+Enter, Ctrl+J, \\ Enter", "a new line in the prompt"),
    ("\u{2190}/\u{2192}, Home/End, Ctrl+A/E, Ctrl+W, Ctrl+U, Alt+K", "move and delete in the prompt, as in a shell"),
    ("Ctrl+F", "search the prompts sent before"),
    ("Ctrl+V", "paste a screenshot (Ctrl+Z takes it back); dropping an image works too"),
    ("Ctrl+K", "every command and menu, searchable"),
    ("Ctrl+B", "move the running shell command to the background"),
    ("Click", "open or fold one thought or tool call"),
    ("Ctrl+D, Ctrl+C twice", "quit"),
    ("Esc", "close, dismiss or interrupt; it never quits"),
];

/// Two sections of key and meaning, the meanings in one column and wrapped
/// under themselves.
fn help_text(width: usize) -> String {
    const HEAD: &str = "\x1b[1;38;2;225;175;95m";
    const KEY: &str = "\x1b[38;2;160;155;145m";
    const WHAT: &str = "\x1b[38;2;160;155;145m";
    const OFF: &str = "\x1b[0m";
    let width = width.max(30);
    let key_w = (width / 3).clamp(12, 26).min(width);
    let what_w = width.saturating_sub(key_w + 4).max(16);
    let mut out = Vec::new();
    for (title, rows) in [
        (
            tr("Commands and skills \u{b7} Tab completes them, Ctrl+K searches everything"),
            HELP_COMMANDS,
        ),
        (tr("Keys"), HELP_KEYS),
    ] {
        if !out.is_empty() {
            out.push(String::new());
        }
        out.push(format!("{HEAD}{title}{OFF}"));
        for (key, what) in rows {
            // The meaning follows the language of the interface; the command
            // and the key stay as the terminal spells them.
            let what = tr(what);
            let wrapped = wrap_plain(what, what_w);
            let key_cells = visible_width(key);
            // A key too long for its column gets a line of its own.
            let mut first = if key_cells > key_w {
                out.push(format!("  {KEY}{key}{OFF}"));
                None
            } else {
                Some(format!("  {KEY}{key}{OFF}{}", " ".repeat(key_w - key_cells + 2)))
            };
            for row in wrapped {
                let lead = first.take().unwrap_or_else(|| " ".repeat(key_w + 4));
                out.push(format!("{lead}{WHAT}{row}{OFF}"));
            }
        }
    }
    out.join("\n")
}

/// `/name arg` as (name, arg); the line starts with a slash.
///
/// The one place that reads a command line: the slash is cut here and
/// nowhere else, so the name and the argument mean the same thing in every
/// path that takes a command.
fn split_command(line: &str) -> (&str, &str) {
    let rest = line.strip_prefix('/').unwrap_or(line);
    match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, arg.trim()),
        None => (rest, ""),
    }
}

/// The paths in a message that attach a picture, and nothing else.
///
/// Only a message that is nothing but paths attaches them: a file named in a
/// sentence is a mention, and the model has `view_image` for it. Attaching it
/// anyway spends the traffic, the tokens and the money without anyone asking
/// for a picture.
fn standalone_paths(text: &str) -> Option<Vec<&str>> {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() || !words.iter().all(|word| word.contains('/') || word.contains('\\')) {
        return None;
    }
    Some(words)
}

/// What a command does when it is typed while a turn runs.
#[derive(Debug, PartialEq)]
enum WhileRunning {
    /// Not a command: a path, or guidance for the model.
    Text,
    /// Nothing it reads or changes belongs to the running turn.
    Now,
    /// It changes the conversation, which the turn holds until it ends.
    AfterTurn,
    /// It would start or end something under the turn.
    Refuse,
    Unknown,
}

fn while_running(name: &str) -> WhileRunning {
    match name {
        "help" | "?" | "whatsnew" | "changelog" | "agents" | "memory" | "memories" | "context" | "verbose" | "expand" | "think"
        | "o" | "diff" | "commit" | "settings" | "config" | "sampling" | "params" | "effort" | "thinking" | "t"
        | "model" | "models" | "m" | "mode" | "prompt" | "skills" | "tools" | "notices" | "log" | "safe" | "rules" | "usage" | "theme" => {
            WhileRunning::Now
        }
        "compact" | "clear" | "export" | "rewind" => WhileRunning::AfterTurn,
        "goal" | "resume" | "regenerate" | "retry" | "update" | "channel" | "mcp" | "editor" | "exit" | "quit" | "q"
        | "uninstall" | "migrate-memory" => WhileRunning::Refuse,
        _ if name.starts_with("skill:") => WhileRunning::Refuse,
        _ if name.is_empty() || name.contains(['/', '\\', '.']) => WhileRunning::Text,
        _ if find_skill_file(name).is_some() => WhileRunning::Refuse,
        _ => WhileRunning::Unknown,
    }
}

/// The first sentence of a tool's description: the list is a list, not a
/// manual.
fn first_sentence(text: &str) -> String {
    let text = text.trim();
    match text.find(". ") {
        Some(at) => text[..at + 1].to_string(),
        None => text.chars().take(90).collect(),
    }
}

/// The format and the path of an export: `/export md ~/notes/chat.md` writes
/// markdown there, `/export "~/my notes/chat.md"` writes markdown to a path
/// with spaces in it.
fn split_export_arg(arg: &str) -> (String, Option<String>) {
    let arg = arg.trim();
    if let Some(rest) = arg.strip_prefix('"') {
        let path = rest.strip_suffix('"').unwrap_or(rest);
        return ("md".to_string(), Some(path.trim().to_string()));
    }
    match arg.split_once(char::is_whitespace) {
        Some((format, rest)) => {
            let path = rest.trim();
            (format.to_string(), (!path.is_empty()).then(|| path.to_string()))
        }
        None => (arg.to_string(), None),
    }
}

/// Where an export goes: the path the person named, with the format's ending
/// added when they left it out, and the default name when they named a
/// folder.
fn export_target(path: &str, default_name: &str) -> String {
    let expanded = flashagent_core::paths::expand_home(path.trim());
    let raw = expanded.as_ref();
    let looks_like_a_folder = raw.ends_with('/') || raw.ends_with('\\') || std::path::Path::new(raw).is_dir();
    if looks_like_a_folder {
        return std::path::Path::new(raw).join(default_name).display().to_string();
    }
    let candidate = std::path::Path::new(raw);
    if candidate.extension().is_none() {
        // No ending: the format's one is used, so "notes/chat" is a markdown file.
        let default_ext = std::path::Path::new(default_name).extension().map(|e| e.to_string_lossy().to_string());
        if let Some(ext) = default_ext {
            return format!("{raw}.{ext}");
        }
    }
    raw.to_string()
}

/// Commands that begin a model turn. During a compaction these wait with a
/// message: the history is with the task, and a turn now would start from
/// nothing.
fn starts_a_turn(name: &str, arg: &str) -> bool {
    (name == "goal" && !arg.is_empty())
        || matches!(name, "regenerate" | "retry")
        || name.starts_with("skill:")
        || find_skill_file(name).is_some()
}

fn untracked_list(files: &[&str]) -> String {
    let mut rows: Vec<String> = files.iter().take(8).map(|f| format!("  {f}")).collect();
    if files.len() > 8 {
        rows.push(tr_fill("  \u{2026} and {n} more", &[("n", &(files.len() - 8).to_string())]));
    }
    rows.join("\n")
}

impl App {
    /// Enter on a non-empty prompt with no turn running.
    pub(crate) async fn submit_input(&mut self, cx: &mut LoopCtx<'_>) -> Flow {
        let line = self.input.trim().to_string();
        if line.starts_with('/') {
            let (name, arg) = split_command(&line);
            // A command is a thing the owner typed and may want again. It goes
            // into the same history as a prompt, before it runs; a path such as
            // `/tmp/shot.png` is not a command and stays out.
            if !matches!(while_running(name), WhileRunning::Unknown | WhileRunning::Text) {
                self.remember_prompt(&line);
            }
            // The history is with the compaction task: a command that starts a
            // turn would begin from nothing, so it waits with a message.
            if self.compacting && starts_a_turn(name, arg) {
                return self.queue_while_compacting(&line);
            }
            if let Some(flow) = self.run_command(cx, name, arg).await {
                return flow;
            }
        }
        if self.compacting {
            return self.queue_while_compacting(&line);
        }
        self.send_prompt(cx);
        Flow::Next
    }

    /// Enter while the context is being compacted: the history is with the
    /// task, so a new turn would start from nothing. The words wait and go out
    /// together once the summary is back.
    fn queue_while_compacting(&mut self, line: &str) -> Flow {
        let text = line.trim().to_string();
        if !text.is_empty() || !self.attachments.is_empty() {
            self.compacted_messages.push((text, std::mem::take(&mut self.attachments)));
        }
        self.input.take();
        self.notice(tr("Compaction is still running \u{b7} your message is queued and will be sent when it finishes"));
        Flow::Next
    }

    /// The compaction came back: whatever was typed while it ran goes out now,
    /// ahead of a draft typed since. Called from the `Compacted` handler
    /// (`on_compacted`) once the history is restored, whether the summary was
    /// saved or not. With no turn under way it is one new turn; with one
    /// running, the words ride it as steering.
    pub(crate) fn send_queued_after_compaction(&mut self, cx: &LoopCtx<'_>) {
        let queued = std::mem::take(&mut self.compacted_messages);
        if queued.is_empty() { return; }
        let joined = queued.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>().join("\n\n");
        let pictures: Vec<Attachment> = queued.into_iter().flat_map(|(_, pictures)| pictures).collect();
        if self.running {
            if let Some(tx) = self.active_steer_tx.clone() {
                let steer = flashagent_core::Steer { text: joined.clone(), images: pictures.iter().map(|p| p.data_url.clone()).collect() };
                if tx.send(steer).is_ok() {
                    self.pending_steers.push((joined, pictures));
                } else {
                    self.compacted_messages.push((joined, pictures));
                }
            } else {
                self.compacted_messages.push((joined, pictures));
            }
            self.renderer.request_reprint();
            return;
        }
        let draft = self.input.take();
        let draft_pictures = std::mem::take(&mut self.attachments);
        self.input.set(joined);
        self.attachments = pictures;
        self.send_prompt(cx);
        self.input.set(draft);
        self.attachments = draft_pictures;
        self.renderer.request_reprint();
    }

    /// Enter on a `/command` while a turn runs. False when the line is not one
    /// (a path like `/tmp/shot.png` is guidance for the model).
    pub(crate) async fn steer_command(&mut self, cx: &mut LoopCtx<'_>) -> bool {
        let line = self.input.trim().to_string();
        let (name, arg) = split_command(&line);
        match while_running(name) {
            WhileRunning::Text => return false,
            WhileRunning::Now => {
                self.remember_prompt(&line);
                self.run_command(cx, name, arg).await;
            }
            WhileRunning::AfterTurn => {
                self.remember_prompt(&line);
                self.input.clear();
                if !self.queued_commands.contains(&line) {
                    self.queued_commands.push(line.clone());
                    // A queued command that says nothing looks like the app
                    // swallowed it, and that reads the same as a hang.
                    self.say(BackgroundNotice::fading(
                        tr_fill(
                            "/{name} will run when this turn finishes \u{b7} Esc ends the turn now instead",
                            &[("name", name)],
                        ),
                        6,
                    )
                    .warning());
                }
            }
            WhileRunning::Refuse => self.say(
                BackgroundNotice::fading(
                    tr_fill(
                        "/{name} does not run while the model works \u{b7} Esc interrupts the turn first",
                        &[("name", name)],
                    ),
                    6,
                )
                .warning(),
            ),
            WhileRunning::Unknown => self.unknown_command(name),
        }
        self.renderer.request_reprint();
        true
    }

    /// What was typed during the turn and waited for it: the conversation is
    /// whole again. A draft in the prompt is not touched.
    pub(crate) async fn run_queued_commands(&mut self, cx: &mut LoopCtx<'_>) {
        let queued = std::mem::take(&mut self.queued_commands);
        if queued.is_empty() {
            return;
        }
        let draft = self.input.take();
        for line in queued {
            let (name, arg) = split_command(&line);
            self.input.set(line.clone());
            self.run_command(cx, name, arg).await;
        }
        self.input.set(draft);
    }

    /// `None` when it is a path like `/tmp/shot.png`, which goes to the model.
    pub(crate) async fn run_command(&mut self, cx: &mut LoopCtx<'_>, name: &str, arg: &str) -> Option<Flow> {
        // A command takes the line; a typo leaves it to fix.
        let typed = self.input.take();
        self.autocomplete_idx = 0;
        match name {
            "help" | "?" => {
                let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                self.chat.push_system(&help_text(w as usize));
            }
            "goal" if arg.is_empty() => self.show_goal_usage(),
            "goal" => self.start_goal(cx, arg),
            "skills" => self.list_skills(),
            "settings" | "config" => {
                let view = self.runtime_settings(cx.perm.state().mode(), cx.source);
                self.open_overlay(Overlay::Settings(Box::new(view)));
            }
            "theme" if arg.is_empty() => self.open_theme_menu(None),
            "theme" => {
                if self.config.select_theme(arg) {
                    self.save_config();
                } else {
                    self.input.set(typed);
                    self.say(BackgroundNotice::fading(
                        tr_fill("Unknown color theme: {name}; /theme lists the available themes", &[("name", arg)]), 8,
                    ).warning());
                }
            }
            "usage" => self.open_usage(cx),
            "whatsnew" | "changelog" => self.show_whatsnew(cx).await,
            "agents" => self.open_agents_view(None),
            "migrate-memory" => self.migrate_memory(cx, arg),
            "memory" | "memories" => {
                let mut modal = open_memory_modal();
                if matches!(arg, "summary" | "s") && !modal.rows.is_empty() {
                    modal.tab = flashagent_tui::memory_view::MemoryTab::Summary;
                    if modal.summary_is_stale() {
                        modal.summary = flashagent_tui::memory_view::SummaryState::Writing;
                        spawn_summary(cx.source.clone(), modal.rows.clone(), cx.tx.clone());
                    }
                }
                self.open_overlay(Overlay::Memory(modal));
            }
            "context" => {
                update_context_usage(&self.current_model, &mut self.context_usage, &self.history, &self.memory_block, &self.chat, cx.perm);
                self.open_overlay(Overlay::Context(ContextModal::new(self.context_usage.clone())));
            }
            "clear" => self.clear_chat(),
            "regenerate" | "retry" => {
                if !self.regenerate(cx) {
                    self.notice(tr("No previous turn to regenerate"));
                }
            }
            "effort" | "thinking" | "t" if arg.is_empty() => self.open_effort_menu(cx.source),
            "effort" | "thinking" | "t" => self.set_effort(cx, arg),
            "model" | "models" | "m" => {
                self.open_model_menu(cx.source);
                if let Some(Overlay::Model(menu)) = self.overlay.as_mut() {
                    arg.chars().for_each(|c| menu.push_filter_char(c));
                }
            }
            "provider" | "providers" => self.provider_command(cx, arg),
            "sampling" | "params" => self.open_overlay(Overlay::Sampling(SamplingView::new(&self.config))),
            "mode" => self.mode_command(cx, arg),
            "prompt" => self.prompt_command(cx, arg),
            "uninstall" => {
                self.uninstall_confirm = true;
                self.suggested_prompt = None;
                self.renderer.request_reprint();
            }
            "resume" => self.open_session_picker(cx.cwd_display, cx.session_id),
            "update" => {
                if flashagent_svc::updater::is_dev_mode() {
                    self.chat.push_system("  \x1b[38;2;225;175;95mAuto-updater is disabled in dev mode\x1b[0m (running from source repository / cargo build).\n  To update your dev build, pull latest git commits and run `cargo build --release`.");
                } else {
                    // Watches one already under way, or starts one.
                    self.start_or_watch_update(cx);
                }
            }
            "channel" => self.channel_command(cx, arg),
            "mcp" => self.mcp_command(cx, arg).await,
            "tasks" | "bg" => self.open_tasks(cx),
            "compact" => self.compact_command(cx, arg),
            "verbose" | "expand" | "think" | "o" => self.verbose_command(arg),
            "exit" | "quit" | "q" => return Some(Flow::Quit),
            "editor" => {
                match open_in_external_editor("", &self.config.external_editor) {
                    Ok(edited) => self.input.set(edited),
                    Err(err) => self.notice(tr_fill("Failed to launch external editor: {err}", &[("err", &err.to_string())])),
                }
                // Coming back to the alternate screen found it blank.
                self.renderer.request_reprint();
            }
            "diff" => self.git_diff_summary(),
            "rewind" => self.rewind_command(cx, arg),
            "commit" => self.git_commit(arg),
            "export" => self.export_conversation(cx, arg),
            "tools" => self.tools_command(cx),
            "notices" | "log" => self.notices_command(),
            "rules" => self.rules_command(cx),
            "safe" => self.safe_command(cx),
            _ => {
                if let Some(skill) = name.strip_prefix("skill:") {
                    self.run_skill(cx, skill);
                } else if arg.is_empty() && find_skill_file(name).is_some() {
                    self.run_skill(cx, name);
                } else if name.is_empty() || name.contains(['/', '\\', '.']) {
                    self.input.set(typed);
                    return None;
                } else {
                    self.input.set(typed);
                    self.unknown_command(name);
                }
            }
        }
        self.renderer.request_reprint();
        Some(Flow::Continue)
    }

    /// "/hlep" is a typo, not a question for the model.
    fn unknown_command(&mut self, name: &str) {
        let word = format!("/{name}");
        let known = flashagent_tui::autocomplete::builtin_commands();
        let triggers: Vec<String> = known.iter().map(|c| c.trigger.clone()).collect();
        let guess = flashagent_tui::autocomplete::closest_match(&word, triggers.iter().map(|t| t.as_str()));
        // The typo stays in the prompt; the placeholder only shows on an empty one.
        let said = match guess {
            Some(g) => tr_fill("Unknown command {word} · did you mean {g}?", &[("word", &word), ("g", g)]),
            None => tr_fill("Unknown command {word} · /help lists them", &[("word", &word)]),
        };
        self.say(BackgroundNotice::fading(said, 6));
    }

    /// Remembers a switch for the first message that uses it. Picking a model
    /// in a menu changes the next request, but writing the rule now would put
    /// it above a chat nothing has been sent in yet.
    pub(crate) fn remember_switch(&mut self, provider: Option<&str>, model: bool, effort: bool) {
        if let Some(name) = provider {
            self.pending_switch.provider = Some(name.to_string());
        }
        self.pending_switch.model |= model;
        self.pending_switch.effort |= effort;
    }

    /// Writes the remembered switch above the message that is about to go out.
    /// One rule even when the provider and its model changed together.
    pub(crate) fn flush_switch_notice(&mut self, source: &BackendSource) {
        let pending = std::mem::take(&mut self.pending_switch);
        let Some(label) = pending.label(&self.model_display(source, &self.current_model), &self.current_effort) else { return };
        if self.chat.push_model_notice(&label) {
            self.renderer.request_reprint();
        }
    }

    fn send_prompt(&mut self, cx: &LoopCtx<'_>) {
        // A new message is a new intent: a reconnect timer from the failed
        // turn must not fire into it.
        self.retry_after_error = false;
        self.auto_retry_attempt = 0;
        self.retry_generation = self.retry_generation.wrapping_add(1);
        self.reconnecting = None;
        let text = self.input.take();
        self.remember_prompt(&text);
        self.history_index = None;
        self.current_draft.clear();
        self.last_expanded = false;

        // Only a path typed out on its own attaches a picture; a file named in a
        // sentence is a mention, and the model has view_image for it. A dropped file
        // arrives as a paste on Unix and as typing on Windows; both land here.
        //
        // The whole message has to be paths: one of them, or several. A path
        // named in a sentence is a mention: attaching it would spend the
        // traffic, the tokens and the money without anyone asking for a picture.
        //
        // Whether every path in the message became a picture. One that does not
        // exist is not a picture, and its name stays in the text: the model is
        // told about a file it cannot see otherwise.
        let mut all_pictures = false;
        if let Some(words) = standalone_paths(&text) {
            all_pictures = true;
            for word in words {
                if self.attachments.len() >= 8 {
                    all_pictures = false;
                    continue;
                }
                match Attachment::from_dropped_path(word) {
                    Some(att) => {
                        if !self.attach(att) {
                            all_pictures = false;
                        }
                    }
                    None => all_pictures = false,
                }
            }
        }
        // A message of only pictures: the dropped path is not text for the model.
        let text = if all_pictures && !self.attachments.is_empty() { String::new() } else { text };
        let shown = if self.attachments.is_empty() {
            text.clone()
        } else {
            // Otherwise the answer refers to something invisible.
            let labels: Vec<String> = self.attachments.iter().map(|a| a.label()).collect();
            if text.trim().is_empty() {
                format!("[{}]", labels.join(", "))
            } else {
                format!("{text}  [{}]", labels.join(", "))
            }
        };
        self.flush_switch_notice(cx.source);
        self.chat.push_user(&shown);
        let mut user_msg = ChatMessage::user(text);
        if !self.attachments.is_empty() {
            user_msg.images = self.attachments.drain(..).map(|a| a.data_url).collect();
        }
        self.history.push(user_msg);
        self.begin_snapshot_turn(cx);
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
    }

    /// Before the message just pushed onto the history.
    fn begin_snapshot_turn(&self, cx: &LoopCtx<'_>) {
        if let (Some(store), Some(msg)) = (cx.perm.state().snapshots(), self.history.last()) {
            store.begin_turn(if msg.content.is_empty() { "[image]" } else { &msg.content });
        }
    }

    fn show_goal_usage(&mut self) {
        let refused = if self.config.permission_mode.is_yolo() {
            // Saying otherwise would describe a filter YOLO has already
            // turned off.
            String::new()
        } else {
            " (dangerous shell commands are refused)".to_string()
        };
        self.chat.push_system(&format!(
            "Autonomous Goal Mode:\n\
             Usage: /goal <task description>\n\
             Example: /goal Refactor error handling in the core crate and run the test suite\n\
             Limits: {}. Change them in Settings → Goal.\n\
             In goal mode FlashAgent approves its own tool calls{refused}, \
             thinks at maximum effort and works on its own. A question it asks you waits {} for an answer, \
             then the run carries on without it.",
            GoalBudgets::from_config(&self.config).summary(),
            flashagent_tui::goal::human_duration(flashagent_tools::GOAL_QUESTION_TIMEOUT)
        ));
    }

    fn start_goal(&mut self, cx: &LoopCtx<'_>, task: &str) {
        let task_desc = match flashagent_tui::goal::parse_goal_task(task) {
            Ok(task) => task,
            Err(msg) => {
                self.notice(msg);
                return;
            }
        };
        let goal_budgets = GoalBudgets::from_config(&self.config);
        // The state card stays above the goal: it says what runs now, and a
        // goal changes its lines rather than taking it away.

        // Restored when the goal ends.
        self.goal_state = Some(SavedGoalState {
            mode: cx.perm.state().mode(),
            effort: self.current_effort.clone(),
            max_steps: self.max_steps,
            task: task_desc.clone(),
        });
        self.goal_ledger = Some(GoalLedger::new(task_desc.clone(), goal_budgets.clone()));

        // A /goal run approves its own calls instead of asking. YOLO is the
        // stronger promise the owner already made: the run keeps it rather
        // than stepping down to Accept All's still-refused commands.
        let goal_mode = if cx.perm.state().mode().is_yolo() { PermissionMode::Yolo } else { PermissionMode::Bypass };
        cx.perm.state().set_mode(goal_mode);
        cx.perm.state().set_goal_active(true);
        self.current_effort = "high".to_string();
        cx.tools_arc.set_goal_mode(true);
        self.goal_active = true;
        self.apply_personality();
        self.refresh_welcome_state(cx.source, cx.mascot_mood);
        // A switch made before the goal belongs above the goal card, not inside it.
        self.flush_switch_notice(cx.source);

        let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
        let card_w = flashagent_tui::content_width(w as usize);
        let inner_w = card_w.saturating_sub(2);
        let border_color = "\x1b[38;2;225;175;95m";
        let reset = "\x1b[0m";

        let title = format!(" Goal: {} ", flashagent_tui::truncate_middle(&task_desc, inner_w.saturating_sub(10)));
        let dash_count = inner_w.saturating_sub(visible_width(&title) + 1);
        self.chat.push_line(
            LineKind::System,
            format!("{border_color}╭─\x1b[1;38;2;232;227;218m{title}{border_color}{}╮{reset}", "─".repeat(dash_count)),
        );
        let meta = flashagent_tui::i18n::tr("Accept All \u{b7} thinking high \u{b7} Esc stops the run");
        let budget = format!("Budget: {}", goal_budgets.summary());
        let rows: Vec<String> =
            [meta, budget.as_str()].iter().flat_map(|text| wrap_plain(text, inner_w.saturating_sub(2))).collect();
        for row in &rows {
            let pad_len = inner_w.saturating_sub(visible_width(row) + 1);
            self.chat.push_line(
                LineKind::System,
                format!("{border_color}│{reset} \x1b[38;2;160;155;145m{row}\x1b[0m{border_color}{}│{reset}", " ".repeat(pad_len)),
            );
        }
        self.chat.push_line(LineKind::System, format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)));

        self.chat.push_user(&format!("/goal {task_desc}"));

        let budget_rule = if goal_budgets.is_unlimited() {
            flashagent_tui::i18n::tr(
                "This run has no budget: it ends when you finish or the user stops it. Say plainly \
                 what is left unfinished or unverified rather than claiming success.",
            )
            .to_string()
        } else {
            format!(
                "Budget for this run: {}. When it runs out the run is stopped wherever it \
                 is, so do the load-bearing work first and say plainly what is left \
                 unfinished or unverified rather than claiming success.",
                goal_budgets.summary()
            )
        };
        let autonomous_directive = format!(
            "[AUTONOMOUS GOAL DIRECTIVE]\n\
             You are operating in fully autonomous /goal mode.\n\
             Target goal: {}\n\n\
             Autonomous Rules:\n\
             1. Work on your own: tool actions are pre-approved, except dangerous shell commands \
             (force pushes, hard resets, recursive force deletes, sudo), which are refused. Ask the \
             user with ask_user only when you are truly blocked on a decision that is theirs; if no \
             answer comes within {}, pick the most reasonable option yourself, carry on, and name \
             that choice in your summary.\n\
             2. Plan, research, edit, execute, and verify completely on your own.\n\
             3. Thoroughly test and verify your changes before finishing.\n\
             4. Conclude with a clear structured summary of what was accomplished.\n\
             5. Record the whole step-by-step list with the plan tool's `create` \
             action before starting, and only send `change` when the list itself \
             changes; mark every step `done` when it finishes: the person who \
             started this run watches it live and has no other way to see where \
             the run stands.\n\n\
             {budget_rule}",
            task_desc,
            flashagent_tui::goal::human_duration(flashagent_tools::GOAL_QUESTION_TIMEOUT),
        );
        self.history.push(ChatMessage::user(autonomous_directive));
        self.begin_snapshot_turn(cx);
        self.start_turn(cx, goal_budgets);
    }

    fn list_skills(&mut self) {
        let skills = flashagent_tui::autocomplete::load_skills(std::path::Path::new("."));
        let listed: Vec<String> = skills.iter().map(|s| format!("  • {} — {}", s.trigger, s.description)).collect();
        if listed.is_empty() {
            self.notice(tr("No skills yet · add .agents/skills/<name>.md here, or ~/.flashagent/skills/<name>.md for every project"));
        } else {
            self.chat.push_system(&format!("Skills:\n{}", listed.join("\n")));
        }
    }

    fn run_skill(&mut self, cx: &LoopCtx<'_>, name: &str) {
        let name = name.trim();
        let Some(skill_content) = find_skill_file(name).and_then(|p| std::fs::read_to_string(p).ok()) else {
            // A name that is close to one of the skills is that one, said out
            // loud: a typo should not need a search.
            let skills = flashagent_tui::autocomplete::load_skills(std::path::Path::new("."));
            let names: Vec<String> = skills.iter().map(|s| s.trigger.trim_start_matches("/skill:").to_string()).collect();
            let guess = flashagent_tui::autocomplete::closest_match(name, names.iter().map(|n| n.as_str()));
            let said = match guess {
                Some(near) => tr_fill(
                    "Unknown skill '{name}' · did you mean '{near}'?",
                    &[("name", name), ("near", near)],
                ),
                None => tr_fill("Unknown skill '{name}' · /skills lists them", &[("name", name)]),
            };
            self.notice(said);
            return;
        };
        self.clear_retry_state();
        self.flush_switch_notice(cx.source);
        self.chat.push_user(&format!("/skill:{name}"));
        self.history.push(ChatMessage::user(format!("Execute skill: {name}\n\nSkill Instructions:\n{skill_content}")));
        self.begin_snapshot_turn(cx);
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
    }

    async fn show_whatsnew(&mut self, cx: &mut LoopCtx<'_>) {
        let now = flashagent_svc::updater::current_version();
        // Asked for on purpose: fall back to the last few releases.
        let mut news = flashagent_tui::whatsnew::since(self.config.last_seen_version.as_deref(), now);
        if news.is_empty() {
            news = flashagent_tui::whatsnew::latest(3);
        }
        if news.is_empty() {
            self.notice(tr("No changelog is bundled with this build"));
        } else {
            let mut deferred = Vec::new();
            flashagent_tui::whatsnew::run_channel(news, now, cx.rx, &mut deferred).await.ok();
            for ev in deferred {
                let _ = cx.tx.send(ev);
            }
            // It drew over the whole screen.
            self.renderer.request_reprint();
        }
    }

    fn clear_chat(&mut self) {
        self.cancel_recap();
        self.chat.clear();
        self.renderer.scroll_to_bottom();
    }

    /// `/usage`: the session's money and tokens, the saved sessions, the
    /// lifetime counter and the providers' own limits, in one tabbed card.
    /// The numbers are a reading taken when it opens, not a live counter.
    fn open_usage(&mut self, cx: &LoopCtx<'_>) {
        use flashagent_tui::usage_view::{SessionUsage, UsageView};
        let session = SessionUsage {
            provider: self.config.active_profile().name.clone(),
            model: self.model_display(cx.source, &self.current_model),
            totals: self.cost.totals().clone(),
            upstream: self.cost.upstream_total(),
            allowance: self.cost.allowance(),
            priced_requests: self.cost.priced_turns(),
            context: self.context_usage.clone(),
            turn_out: self.token_tracker.retained_output(),
            turn_agents: self.token_tracker.subagent_output(),
        };
        let providers: Vec<String> = self
            .config
            .providers
            .iter()
            .map(|p| p.name.clone())
            .filter(|name| !name.trim().is_empty())
            .take(12)
            .collect();
        let pending = !providers.is_empty();
        let history = saved_usage_rows();
        let view = UsageView::new(session, history, self.usage_history.clone(), providers, pending);
        self.open_overlay(Overlay::Usage(Box::new(view)));
        self.refresh_usage_limits(cx);
    }

    /// One bounded request per configured provider. A local server answers
    /// "there is no account" without a round trip; a cloud one is given eight
    /// seconds and then reported as slow, not hidden.
    pub(crate) fn refresh_usage_limits(&self, cx: &LoopCtx<'_>) {
        for profile in self.config.providers.iter().take(12) {
            if profile.url.trim().is_empty() {
                continue;
            }
            let endpoint = profile.endpoint();
            let provider = profile.name.clone();
            let tx = cx.tx.clone();
            tokio::spawn(async move {
                let outcome = flashagent_llm::limits::fetch(&endpoint).await;
                let _ = tx.send(UiEvent::UsageLimits { provider, outcome });
            });
        }
    }

    fn set_effort(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let wanted = arg.to_lowercase();
        let mut known: Vec<String> = ["auto", "default", "off", "low", "medium", "high"].iter().map(|s| s.to_string()).collect();
        if let Some(p) = cx.source.profile() {
            known.extend(p.presets.iter().filter(|p| !known.contains(p)).cloned().collect::<Vec<_>>());
        }
        if !known.contains(&wanted) {
            self.notice(tr_fill("Unknown effort '{wanted}' · try {known}", &[("wanted", &wanted), ("known", &known.join(", "))]));
            return;
        }
        let changed = self.current_effort != wanted;
        self.current_effort = wanted;
        self.save_live_effort();
        self.refresh_welcome_state(cx.source, cx.mascot_mood);
        if changed {
            self.remember_effort_switch();
        }
    }

    fn mode_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let mode = match arg.to_lowercase().as_str() {
            "" => cx.perm.state().mode().next_with_yolo(self.config.yolo_mode),
            "planning" | "plan" => PermissionMode::Planning,
            "manual" | "man" => PermissionMode::Manual,
            "acceptedits" | "accept_edits" | "edits" | "auto" | "default" => PermissionMode::AcceptEdits,
            "bypass" | "accept_all" | "all" => PermissionMode::Bypass,
            "yolo" => {
                // The mode is armed in Settings, not typed into being: a
                // command alone must not turn every filter off.
                if !self.config.yolo_mode {
                    self.notice(tr("YOLO is not enabled: turn it on in Settings → General first"));
                    return;
                }
                PermissionMode::Yolo
            }
            "autonomic" => {
                self.notice(tr("Autonomous mode runs only inside /goal <task>"));
                return;
            }
            _ => {
                self.notice(tr("Usage: /mode planning | /mode manual | /mode edits | /mode all | /mode yolo"));
                return;
            }
        };
        cx.perm.state().set_mode(mode);
        self.remember_mode(mode);
        self.refresh_welcome(cx.source, cx.mascot_mood);
        self.notice(format!("{} {}", tr("Permission mode set to:"), tr(mode.label())));
        if mode.is_yolo() {
            self.say(BackgroundNotice::fading(
                tr("YOLO: no filters · any tool and any command runs without asking"),
                8,
            ));
        }
    }

    fn channel_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        use flashagent_core::config::UpdateChannel;
        let wanted = match arg.to_lowercase().as_str() {
            "" => {
                self.chat.push_system(&format!(
                    "Current release channel: \x1b[1m{}\x1b[0m\nUsage: /channel <stable|beta>\n• /channel stable — Official stable releases (v*)\n• /channel beta   — Latest beta pre-releases (b*)",
                    self.config.update_channel.label()
                ));
                return;
            }
            "stable" | "v" => UpdateChannel::Stable,
            "beta" | "b" => UpdateChannel::Beta,
            _ => {
                self.notice(tr("Unknown channel · /channel stable or /channel beta"));
                return;
            }
        };
        if wanted == self.config.update_channel {
            self.notice(tr_fill("Already on the {channel} channel", &[("channel", wanted.label())]));
            return;
        }
        // Switching can install an older binary, so it always asks.
        self.ask_channel_switch(cx, wanted);
    }

    async fn mcp_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let (sub, rest) = match arg.split_once(char::is_whitespace) {
            Some((sub, rest)) => (sub, rest.trim()),
            None => (arg, ""),
        };
        match sub {
            "" | "help" => self.open_mcp(cx, McpViewTab::Overview).await,
            "list" | "ls" => self.open_mcp(cx, McpViewTab::Servers).await,
            "market" | "marketplace" => self.open_mcp(cx, McpViewTab::Marketplace).await,
            "test" if rest.is_empty() => self.chat.push_system(flashagent_tui::i18n::tr(
                "Usage: /mcp test <server_name>\nExample: /mcp test sqlite",
            )),
            "test" => {
                self.notice(tr_fill("Testing MCP server '{name}'…", &[("name", rest)]));
                // Drawn before the wait, which can take a while.
                self.draw(cx, None);
                match cx.tools_arc.mcp_manager().test_server(rest).await {
                    Ok(report) => {
                        let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                        for line in flashagent_tui::mcp_view::render_mcp_test_report(&report, w as usize) {
                            self.chat.push_line(LineKind::System, line);
                        }
                        self.custom_placeholder = None;
                    }
                    Err(e) => {
                        self.chat.push_system(&format!("\x1b[38;2;230;110;95mMCP server '{rest}' test failed:\x1b[0m\n{e}"));
                        self.custom_placeholder = None;
                    }
                }
            }
            "add" if rest.is_empty() => self.chat.push_system(
                flashagent_tui::i18n::tr(
                    "Usage: /mcp add <marketplace_id>\nExample: /mcp add sqlite\nType /mcp market to browse extensions.",
                ),
            ),
            "add" => {
                let Some(item) = flashagent_tools::mcp::find_marketplace_item(rest) else {
                    self.notice(tr_fill("Unknown extension '{name}' · /mcp market lists them", &[("name", rest)]));
                    return;
                };
                let cfg = flashagent_tools::mcp::scaffold_config(item);
                match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                    Ok(path) => {
                        let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                        for line in flashagent_tui::mcp_view::render_mcp_add_success(item, &path, w as usize) {
                            self.chat.push_line(LineKind::System, line);
                        }
                        let mgr = cx.tools_arc.mcp_manager();
                        let server_id = item.id.to_string();
                        tokio::spawn(async move {
                            let _ = mgr.reload().await;
                            let _ = mgr.start_server(&server_id).await;
                        });
                    }
                    Err(e) => self.notice(tr_fill("Failed to save the MCP configuration: {e}", &[("e", &e.to_string())])),
                }
            }
            "reload" => {
                let mgr = cx.tools_arc.mcp_manager();
                match mgr.reload().await {
                    Ok(()) => {
                        let statuses = mgr.server_status_list().await;
                        let active = statuses.iter().filter(|s| s.state == flashagent_tools::mcp::ServerConnectionState::Active).count();
                        let tools: usize = statuses.iter().map(|s| s.tool_count).sum();
                        self.notice(tr_fill(
                            "MCP reloaded · {servers} · {tools}",
                            &[
                                (
                                    "servers",
                                    &flashagent_tui::text::plural(active, "active server", "active servers"),
                                ),
                                (
                                    "tools",
                                    &flashagent_tui::text::plural(tools, "tool", "tools"),
                                ),
                            ],
                        ));
                    }
                    Err(e) => self.notice(tr_fill("MCP reload failed: {e}", &[("e", &e.to_string())])),
                }
            }
            _ => self.notice(tr("Usage: /mcp list | market | test <server> | add <id> | reload")),
        }
    }

    /// Compaction asks the model to read the whole conversation and write it
    /// back short. On a laptop model that takes minutes, so it runs on its own
    /// task: the interface keeps drawing, keeps taking keys, and the line under
    /// the prompt says what is happening instead of the app looking frozen.
    fn compact_command(&mut self, cx: &LoopCtx<'_>, focus: &str) {
        self.compact_command_with_growth(cx, focus, true);
    }

    pub(crate) fn compact_command_with_growth(&mut self, cx: &LoopCtx<'_>, focus: &str, allow_growth: bool) {
        if self.compacting {
            self.notice(tr("A compaction is already running"));
            return;
        }
        if self.running {
            // The history belongs to the turn in flight: cutting it now would
            // orphan its tool calls and the turn could never finish.
            self.notice(tr("/compact waits for the turn to finish · Esc ends the turn first"));
            return;
        }
        let Some(archive) = session_path(cx.session_id) else {
            self.chat.push_system("Compaction failed or saved no space · conversation unchanged");
            return;
        };
        // One exchange is enough: the person asked for a compaction, and the
        // summary of a short conversation is still a summary.
        let has_turn = self.history.iter().any(flashagent_core::is_prompt);
        if !has_turn {
            self.chat.push_system("Nothing to compact yet");
            return;
        }

        self.custom_placeholder = None;
        self.suggested_prompt = None;
        self.compacting = true;
        self.chat.start_compaction_card();
        self.renderer.request_reprint();

        // Keep the original available for autosave and cancellation. New turns
        // wait while the task owns a separate working copy.
        let mut history = self.history.clone();
        // What the window holds now: the number the summary is measured
        // against, and what the card says it freed.
        let used = self.context_usage.total_used();
        // The room one request may carry: the window minus the prompt, the
        // memory and the schemas (once). What the conversation holds now is
        // what gets summarized, so it is not subtracted here.
        let room = crate::compact::room_for(
            self.context_usage.total_capacity,
            self.context_usage.system_tokens + self.context_usage.memory_tokens + self.context_usage.tools_tokens,
        );
        let source = cx.source.clone();
        let focus = (!focus.is_empty()).then(|| focus.to_string());
        // The model in use: what the conversation costs is counted with its
        // own counter, and the compaction decides on the same numbers the bar
        // shows.
        let model = self.current_model.clone();
        let session = cx.session_id.clone();
        let tx = cx.tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.compaction_cancel = Some(cancel.clone());
        tokio::spawn(async move {
            let sink = SummarySink::ui(tx.clone());
            let work = compact_context_with_sink(
                source.as_ref(),
                &mut history,
                crate::compact::Compaction {
                    focus_prompt: focus.as_deref(), archive_path: &archive, real_used: Some(used),
                    room: Some(room), allow_growth, model: &model,
                },
                Some(&sink),
            );
            let result = tokio::select! {
                result = work => result,
                _ = async {
                    while !cancel.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(25)).await; }
                } => None,
            };
            let _ = tx.send(UiEvent::Compacted { session, history, card: result });
        });
    }

    fn verbose_command(&mut self, arg: &str) {
        (self.last_expanded, self.all_expanded) = match arg {
            // none -> last -> all -> none, like F2.
            "" => match (self.last_expanded, self.all_expanded) {
                (false, false) => (true, false),
                (true, false) => (false, true),
                _ => (false, false),
            },
            "all" => (false, true),
            "last" => (true, false),
            "off" | "none" | "collapse" => (false, false),
            _ => {
                self.notice(tr("Usage: /verbose all | /verbose last | /verbose off"));
                return;
            }
        };
    }

    /// The changed lines, not only their count, and the files git does not track yet.
    fn git_diff_summary(&mut self) {
        const SHOWN: usize = 60;
        let git = |args: &[&str]| -> Result<String, std::io::Error> {
            let out = std::process::Command::new("git").args(args).output()?;
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        };
        let (stat, patch, untracked) = match (git(&["diff", "--stat"]), git(&["diff", "--no-color", "-U2"]), git(&["ls-files", "--others", "--exclude-standard"])) {
            (Ok(stat), Ok(patch), Ok(untracked)) => (stat, patch, untracked),
            (Err(e), ..) | (_, Err(e), _) | (.., Err(e)) => {
                self.notice(tr_fill("Failed to run git diff: {e}", &[("e", &e.to_string())]));
                return;
            }
        };
        let untracked: Vec<&str> = untracked.lines().collect();
        if stat.trim().is_empty() {
            let said = match untracked.len() {
                0 => "No changes to tracked files".to_string(),
                1 => "No changes to tracked files \u{b7} 1 untracked file".to_string(),
                n => format!("No changes to tracked files \u{b7} {n} untracked files"),
            };
            if untracked.is_empty() {
                self.notice(said);
                return;
            }
            self.chat.push_system(&format!("{said}:\n{}", untracked_list(&untracked)));
            return;
        }
        let mut out = format!("Git diff:\n{}", stat.trim_end());
        // Its own lines only: the file headers repeat what the stat above says.
        let body: Vec<&str> = patch
            .lines()
            .filter(|l| !(l.starts_with("diff --git") || l.starts_with("index ") || l.starts_with("--- ") || l.starts_with("+++ ")))
            .collect();
        for line in body.iter().take(SHOWN) {
            let clean: String = line.chars().map(|c| if c.is_control() && c != '\t' { ' ' } else { c }).collect();
            let color = match clean.as_bytes().first() {
                Some(b'+') => "\x1b[38;2;145;205;140m",
                Some(b'-') => "\x1b[38;2;230;110;95m",
                Some(b'@') => "\x1b[38;2;135;130;125m",
                _ => "\x1b[38;2;160;155;145m",
            };
            out.push_str(&format!("\n{color}{}\x1b[0m", clean.replace('\t', "    ")));
        }
        if body.len() > SHOWN {
            out.push_str(&format!("\n\x1b[38;2;135;130;125m\u{2026} {} more lines (git diff shows all)\x1b[0m", body.len() - SHOWN));
        }
        if !untracked.is_empty() {
            out.push_str(&format!("\n\nUntracked:\n{}", untracked_list(&untracked)));
        }
        self.chat.push_system(&out);
    }

    fn rewind_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let Some(store) = cx.perm.state().snapshots() else {
            self.notice(tr("Rewind is not available in this session"));
            return;
        };
        let prompts: Vec<String> =
            self.history.iter().filter(|m| flashagent_core::is_prompt(m)).map(|m| m.content.clone()).collect();
        let prompt_refs: Vec<&str> = prompts.iter().map(String::as_str).collect();
        let turns = store.rewindable(&prompt_refs);
        if turns.is_empty() {
            self.notice(tr("Nothing to rewind to yet"));
            return;
        }
        let Some(n) = arg.parse::<usize>().ok().filter(|n| (1..=turns.len()).contains(n)) else {
            if !arg.is_empty() {
                self.notice(tr_fill("No turn {arg} to rewind to: pick 1 to {count}", &[("arg", arg), ("count", &turns.len().to_string())]));
            }
            let mut text = String::from(
                flashagent_tui::i18n::tr(
                    "Turns you can go back to. The files FlashAgent changed and the conversation return to how they were before the turn:\n",
                ),
            );
            for (i, turn) in turns.iter().enumerate() {
                let first_line = extract_user_prompt(&prompts[turn.user_index]).lines().next().unwrap_or("").to_string();
                let files = match turn.files {
                    0 => "no file changes".to_string(),
                    1 => "1 file".to_string(),
                    k => format!("{k} files"),
                };
                text.push_str(&format!("  {}. {} · {files}\n", i + 1, flashagent_tui::truncate_middle(&first_line, 60)));
            }
            text.push_str(flashagent_tui::i18n::tr("Type /rewind <number>. Changes made by shell commands are not undone."));
            self.chat.push_system(&text);
            return;
        };
        if self.running {
            self.notice(tr("Wait for the turn to finish before rewinding"));
            return;
        }
        let target = turns[n - 1].clone();
        let prompt_label = flashagent_tui::truncate_middle(
            extract_user_prompt(&prompts[target.user_index]).lines().next().unwrap_or(""),
            60,
        );
        let files = store.preview_rewind(target.turn).into_iter().map(|f| RewindFileRow::from((store.as_ref(), f))).collect();
        self.open_overlay(Overlay::Rewind(RewindConfirm { target, prompt_label, files, confirm: true }));
    }

    fn git_commit(&mut self, message: &str) {
        let commit_msg = if message.is_empty() {
            let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            format!("chore: checkpoint update ({ts})")
        } else {
            message.to_string()
        };
        match std::process::Command::new("git").args(["commit", "-m", &commit_msg]).output() {
            Ok(out) if out.status.success() => {
                self.chat.push_system(&tr_fill(
                    "Git commit succeeded:\n{out}",
                    &[("out", String::from_utf8_lossy(&out.stdout).trim_end())],
                ));
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                let s = String::from_utf8_lossy(&out.stdout);
                self.chat.push_system(&tr_fill(
                    "Git commit status:\n{out}",
                    &[("out", &format!("{s}{err}"))],
                ));
            }
            Err(e) => self.notice(tr_fill("Failed to run git commit: {e}", &[("e", &e.to_string())])),
        }
    }

    /// `/export <format> [path]`: the format is the first word, the path is
    /// what is left of it. A path with spaces is written in quotes.
    /// `/safe`: the safe mode. Reading and editing inside the project, and
    /// nothing else: no shell, no network, no subagents. It is a mode on top
    /// of the permission mode, not instead of it.
    fn safe_command(&mut self, cx: &LoopCtx<'_>) {
        self.config.safe_mode = !self.config.safe_mode;
        self.apply_disabled_tools(cx);
        self.save_config();
        let said = if self.config.safe_mode {
            "Safe mode on · the shell, the web and the subagents are off; reading and editing inside the project are not"
        } else {
            "Safe mode off · every tool is back"
        };
        self.notice(said);
    }

    /// `/notices`: what the app said, newest first. A notice fades and is
    /// gone; this is where it is read again.
    fn notices_command(&mut self) {
        let entries = self.notice_history();
        self.open_overlay(Overlay::Notices(flashagent_tui::notice_log::NoticeLog::new(entries)));
    }

    /// `/rules`: the standing permissions the owner gave with "Always". They
    /// are kept between the runs, and this is where they are seen and taken
    /// back.
    fn rules_command(&mut self, cx: &LoopCtx<'_>) {
        let view = flashagent_tui::rules_view::RulesView::new(&cx.perm.rules());
        self.open_overlay(Overlay::Rules(view));
    }

    /// `/tools`: the list of tools, and which of them the model may call. A
    /// tool that is off is not in the schemas the model reads, and a call to
    /// one is refused.
    pub(crate) fn tools_command(&mut self, cx: &LoopCtx<'_>) {
        use flashagent_tui::tools_view::{ToolRow, ToolsView};
        let off = cx.perm.disabled_tools();
        let core = true;
        let web = self.config.web_tools;
        let vision = cx.tools_arc.vision_supported();
        let rows: Vec<ToolRow> = flashagent_core::registry::TOOLS
            .iter()
            .map(|def| ToolRow {
                name: def.name.to_string(),
                description: first_sentence(def.description),
                on: !off.iter().any(|n| n == def.name),
                available: flashagent_core::registry::offered(def, core, vision, web, true),
            })
            .collect();
        self.open_overlay(Overlay::Tools(ToolsView::new(rows)));
    }

    fn export_conversation(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        // A file with only a header, in the project folder, helps nobody.
        if !self.history.iter().any(flashagent_core::is_prompt) {
            self.say(BackgroundNotice::fading(tr("Nothing to export yet").to_string(), 5));
            self.renderer.request_reprint();
            return;
        }
        let (format_arg, path_arg) = split_export_arg(arg);
        let format_arg = format_arg.to_lowercase();
        // The id already starts with "session_" (was session_session_1789.md).
        let stem = cx.session_id.strip_prefix("session_").unwrap_or(cx.session_id);
        let text_of = |m: &ChatMessage| {
            if m.content.trim().is_empty() && !m.images.is_empty() {
                let images = flashagent_tui::text::plural(m.images.len(), "attached image", "attached images");
                format!("[{images}]")
            } else {
                m.content.clone()
            }
        };
        let (filename, content) = match format_arg.as_str() {
            "html" => {
                let mut html = String::from("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>FlashAgent Session</title><style>body{font-family:sans-serif;max-width:800px;margin:2rem auto;line-height:1.6;background:#1e1e2e;color:#cdd6f4;}pre{background:#181825;padding:1rem;border-radius:6px;overflow-x:auto;}h3{color:#89b4fa;}</style></head><body>");
                for m in &self.history {
                    let escaped = text_of(m).replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                    html.push_str(&format!("<h3>Role: {}</h3><pre>{escaped}</pre>", m.role.as_str()));
                }
                html.push_str("</body></html>");
                (format!("session_{stem}.html"), html)
            }
            "jsonl" | "json" => {
                let mut buf = String::new();
                for m in &self.history {
                    if let Ok(line) = serde_json::to_string(&SavedMessage::from(m)) {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
                (format!("session_{stem}.jsonl"), buf)
            }
            _ => (
                format!("session_{stem}.md"),
                export::conversation_markdown(&self.history, cx.session_id, &self.current_model, cx.cwd_display),
            ),
        };
        // Where it goes: the path the person named, or the project folder.
        let target = match path_arg {
            Some(p) => export_target(&p, &filename),
            None => filename.clone(),
        };
        // The folders the person named are part of what they asked for.
        if let Some(parent) = std::path::Path::new(&target).parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
        match std::fs::write(&target, content) {
            Ok(_) => {
                let shown = std::fs::canonicalize(&target).map_or(target.clone(), |p| p.display().to_string());
                // It fades: as the prompt's placeholder it came back every time the prompt was emptied.
                self.say(BackgroundNotice::fading(tr_fill("Exported to {path}", &[("path", &shown)]), 8));
                self.renderer.request_reprint();
            }
            Err(e) => self.notice(tr_fill("Failed to export conversation: {e}", &[("e", &e.to_string())])),
        }
    }

    /// Files the turns changed are restored and the prompt goes back into the
    /// input. Only reached after the confirmation card said yes.
    pub(crate) fn commit_rewind(&mut self, cx: &mut LoopCtx<'_>, target: flashagent_core::Rewindable) {
        let Some(store) = cx.perm.state().snapshots() else {
            return;
        };
        let prompts: Vec<String> = self
            .history
            .iter()
            .filter(|m| flashagent_core::is_prompt(m))
            .map(|m| m.content.clone())
            .collect();
        let Some(prompt) = prompts.get(target.user_index).cloned() else {
            return;
        };
        let report = store.rewind(target.turn);
        if !report.failed.is_empty() {
            for (path, why) in &report.failed {
                self.chat.push_line(
                    LineKind::ToolError,
                    tr_fill(
                        "Not put back: {path} — {why}",
                        &[("path", &store.display_path(path)), ("why", why)],
                    ),
                );
            }
            self.notice(tr("Rewind incomplete · history kept; fix the errors above and run /rewind again").to_string());
            self.renderer.request_reprint();
            return;
        }
        let taken_back = prompts.len() - target.user_index;
        let cut = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, m)| flashagent_core::is_prompt(m))
            .nth(target.user_index)
            .map(|(i, _)| i);
        if let Some(cut) = cut {
            self.history.truncate(cut);
        }
        self.chat.truncate_before_nth_last_user(taken_back);
        self.renderer.scroll_to_bottom();
        self.context_usage.invalidate_server();
        update_context_usage(&self.current_model, &mut self.context_usage, &self.history, &self.memory_block, &self.chat, cx.perm);
        self.suggested_prompt = None;
        self.latest_suggestion = None;
        self.custom_placeholder = None;

        // The words, without goal scaffolding or the memory block.
        self.input.set(extract_user_prompt(&prompt).to_string());
        self.autosave(cx.session_id, cx.cwd_display);
        self.renderer.request_reprint();
    }
}

impl App {
    /// `/prompt`: the compact/standard/full choice. Auto is the default and
    /// follows what the running model can take; an explicit choice is kept in
    /// the config for this and later sessions.
    pub(crate) fn prompt_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() {
            let effective = flashagent_core::PromptDetail::auto(
                crate::backend_is_local(cx.source),
                self.context_usage.total_capacity,
                &self.config.prompt_detail,
            );
            self.notice(tr_fill(
                "Prompt: {configured} (this model gets {effective})",
                &[("configured", &self.config.prompt_detail), ("effective", effective.label())],
            ));
            return;
        }
        let chosen = arg.to_ascii_lowercase();
        if chosen != "auto" && flashagent_core::PromptDetail::parse(&chosen).is_none() {
            self.notice(tr_fill(
                "Unknown prompt detail: {name}; use auto, compact, standard or full",
                &[("name", arg)],
            ));
            return;
        }
        self.config.prompt_detail = chosen.clone();
        self.save_config();
        self.refresh_prompt_tier(cx);
        self.notice(tr_fill("Prompt detail: {name}", &[("name", &chosen)]));
        self.renderer.request_reprint();
    }

    /// `/migrate-memory`: the old store is listed, then a confirmed run backs
    /// it up and hands the conversion to the agent. Nothing is deleted until
    /// `cleanup confirm`, and the backup stays.
    pub(crate) fn migrate_memory(&mut self, cx: &mut LoopCtx<'_>, arg: &str) {
        use flashagent_core::memory_store::{self, Scope, Store};
        let cwd = std::env::current_dir().unwrap_or_default();
        let Some(home) = memory_store::home_dir() else {
            self.notice(tr("No home directory to read memory from"));
            return;
        };
        let inventory = memory_store::legacy_inventory(&cwd, &home);
        let argument = arg.trim().to_ascii_lowercase();
        match argument.as_str() {
            "" | "plan" => {
                if inventory.is_empty() {
                    self.notice(tr("There is no old memory to migrate"));
                    return;
                }
                let project = Store::for_scope_at(&home, Scope::Project, &cwd);
                let global = Store::for_scope_at(&home, Scope::Global, &cwd);
                let mut text = tr_fill(
                    "Old memory: {count} entries ({project} in the project, {global} global).",
                    &[
                        ("count", &inventory.count().to_string()),
                        ("project", &inventory.by_scope(Scope::Project).len().to_string()),
                        ("global", &inventory.by_scope(Scope::Global).len().to_string()),
                    ],
                );
                text.push_str(&format!(
                    "\n\nNew project memory: {}\nNew global memory: {}\n\n/migrate-memory run backs the old files up and lets the agent convert them; /migrate-memory verify counts what arrived; /migrate-memory cleanup confirm removes the old files after you have checked. Nothing is deleted before that.",
                    project.dir().display(),
                    global.dir().display()
                ));
                self.chat.push_system(&text);
            }
            "run" => {
                if inventory.is_empty() {
                    self.notice(tr("There is no old memory to migrate"));
                    return;
                }
                let stamp = format!("{}-{}", memory_store::today(), std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0));
                let backup = match memory_store::backup_legacy(&cwd, &home, &stamp) {
                    Ok(path) => path,
                    Err(why) => {
                        self.notice(tr_fill("Could not back up the old memory: {why}", &[("why", &why.to_string())]));
                        return;
                    }
                };
                let prompt = flashagent_tools::memory_tools::migration_prompt(&inventory, &backup);
                self.notice(tr_fill("Old memory backed up to {path}", &[("path", &backup.display().to_string())]));
                // The owner asked for this migration, so the memory calls it
                // needs do not ask one by one. The rule is for this process
                // only; it is not written to the config.
                cx.perm.state().allow_tool_always("memory");
                // An ordinary turn: the agent reads each old entry with the
                // memory tool and imports it. The backup is already safe.
                self.input.set(prompt);
                let _ = cx.tx.send(UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE));
            }
            "verify" => {
                let project = Store::for_scope_at(&home, Scope::Project, &cwd);
                let global = Store::for_scope_at(&home, Scope::Global, &cwd);
                let new_project = project.list().len();
                let new_global = global.list().len();
                self.chat.push_system(&tr_fill(
                    "New memory: {project} project and {global} global topic(s). Old entries still on disk: {old}. A run that left some behind is visible by comparing the two.",
                    &[
                        ("project", &new_project.to_string()),
                        ("global", &new_global.to_string()),
                        ("old", &inventory.count().to_string()),
                    ],
                ));
            }
            "cleanup" | "cleanup confirm" => {
                if argument == "cleanup" {
                    self.chat.push_system(tr(
                        "This removes the old project memory files and the generated index blocks after the backup. Type /migrate-memory cleanup confirm if you have checked /migrate-memory verify.",
                    ));
                    return;
                }
                let backups = home.join(".flashagent").join("memory-backup");
                if !backups.is_dir() {
                    self.notice(tr("Refusing to clean up without a backup: run /migrate-memory run first"));
                    return;
                }
                let mut done = Vec::new();
                match memory_store::strip_legacy_index(&cwd.join(memory_store::INDEX)) {
                    Ok(_) => done.push(cwd.join(memory_store::INDEX).display().to_string()),
                    Err(why) => self.notice(tr_fill("Could not clean {path}: {why}", &[("path", &cwd.join(memory_store::INDEX).display().to_string()), ("why", &why.to_string())])),
                }
                if memory_store::remove_legacy_project_dir(&cwd).unwrap_or(false) {
                    done.push(cwd.join("memory").display().to_string());
                }
                // The old global index is outside the new store; the new
                // global topics are the migrated files and stay.
                let global_old_index = home.join(".flashagent").join(memory_store::INDEX);
                if let Err(why) = memory_store::strip_legacy_index(&global_old_index) {
                    self.notice(tr_fill("Could not clean {path}: {why}", &[("path", &global_old_index.display().to_string()), ("why", &why.to_string())]));
                }
                if done.is_empty() {
                    self.notice(tr("There is no old memory to clean up"));
                } else {
                    self.notice(tr_fill("Cleaned {paths}. The backup under {backup} is kept.", &[
                        ("paths", &done.join(", ")),
                        ("backup", &backups.display().to_string()),
                    ]));
                }
            }
            _ => self.notice(tr("Usage: /migrate-memory [run|verify|cleanup [confirm]]")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every line of /help has a Russian version: the help is the one screen
    /// a user reads when they do not know the app.
    #[test]
    fn every_help_line_is_translated() {
        for (key, what) in HELP_COMMANDS.iter().chain(HELP_KEYS.iter()) {
            assert!(
                flashagent_tui::i18n::translate(flashagent_tui::i18n::Lang::Russian, what) != *what,
                "the help line of {key} is not translated: {what}"
            );
        }
    }

    #[test]
    fn a_command_typed_during_a_turn_runs_waits_or_is_refused_by_what_it_touches() {
        for name in ["help", "verbose", "diff", "mode", "prompt", "effort", "model", "agents", "memory", "context", "usage", "theme"] {
            assert_eq!(while_running(name), WhileRunning::Now, "{name}");
        }
        // The conversation belongs to the turn until it ends.
        for name in ["compact", "clear", "export", "rewind"] {
            assert_eq!(while_running(name), WhileRunning::AfterTurn, "{name}");
        }
        for name in ["goal", "resume", "regenerate", "exit", "uninstall", "skill:review", "update", "migrate-memory"] {
            assert_eq!(while_running(name), WhileRunning::Refuse, "{name}");
        }
        // A path is for the model; a misspelt command is not.
        assert_eq!(while_running("tmp/shot.png"), WhileRunning::Text);
        assert_eq!(while_running(""), WhileRunning::Text);
        assert_eq!(while_running("hlep"), WhileRunning::Unknown);
    }

    /// A picture is attached for a path typed out on its own. A file named in
    /// a sentence is a mention: the model has view_image for it, and attaching
    /// it spends the traffic and the tokens without anyone asking.
    #[test]
    fn only_a_message_that_is_nothing_but_paths_attaches_a_picture() {
        assert_eq!(standalone_paths("/home/me/shot.png"), Some(vec!["/home/me/shot.png"]));
        // Several paths are still only paths.
        assert_eq!(standalone_paths("/home/me/a.png /home/me/b.png"), Some(vec!["/home/me/a.png", "/home/me/b.png"]));
        // A path named in a sentence is a mention.
        assert_eq!(standalone_paths("look at /home/me/shot.png"), None);
        assert_eq!(standalone_paths("shot.png is at /home/me/shot.png"), None);
        // And there is nothing to attach in an ordinary message.
        assert_eq!(standalone_paths("hello"), None);
        assert_eq!(standalone_paths(""), None);
        assert_eq!(standalone_paths("   "), None);
    }

    #[test]
    fn a_command_line_splits_into_name_and_argument() {
        assert_eq!(split_command("/compact keep the schema"), ("compact", "keep the schema"));
        assert_eq!(split_command("/help"), ("help", ""));
        assert_eq!(split_command("/tmp/shot.png"), ("tmp/shot.png", ""));
    }

    /// The slash is cut in one place. It used to be cut by hand where the
    /// command was taken and again inside the splitter, so the two could
    /// disagree about what a command is called.
    #[test]
    fn a_command_is_read_the_same_way_wherever_it_comes_from() {
        for line in ["/compact keep the schema", "/help", "/tmp/shot.png"] {
            let (name, arg) = split_command(line);
            let rest = line.strip_prefix('/').expect("a command line starts with a slash");
            assert_eq!(rest.split_whitespace().next(), Some(name), "{line}");
            assert_eq!(split_command(&line[1..]), (name, arg), "{line}");
        }
    }

    #[test]
    fn an_export_takes_a_format_and_a_path() {
        assert_eq!(split_export_arg("md"), ("md".to_string(), None));
        assert_eq!(split_export_arg("md ~/notes/chat.md"), ("md".to_string(), Some(tr("~/notes/chat.md").to_string())));
        assert_eq!(split_export_arg("  html  out.html "), ("html".to_string(), Some(tr("out.html").to_string())));
        // A path with spaces in it is written in quotes.
        assert_eq!(split_export_arg("\"~/my notes/chat.md\""), ("md".to_string(), Some(tr("~/my notes/chat.md").to_string())));
    }

    #[test]
    fn the_path_of_an_export_keeps_the_format_when_the_person_left_it_out() {
        assert_eq!(export_target("notes/chat", "session_1.md"), "notes/chat.md");
        assert_eq!(export_target("notes/chat.md", "session_1.md"), "notes/chat.md");
        assert_eq!(export_target("notes/chat.jsonl", "session_1.md"), "notes/chat.jsonl");
        // A folder gets the default name inside it.
        assert_eq!(export_target("notes/", "session_1.md"), "notes/session_1.md");
    }

    #[test]
    fn only_a_turn_command_waits_for_compaction() {
        assert!(starts_a_turn("goal", "ship it"));
        assert!(!starts_a_turn("goal", ""));
        assert!(starts_a_turn("regenerate", ""));
        assert!(starts_a_turn("retry", ""));
        assert!(starts_a_turn("skill:review", ""));
        assert!(!starts_a_turn("compact", ""));
        assert!(!starts_a_turn("help", ""));
    }

    #[test]
    fn messages_queued_during_compaction_keep_their_order_and_leave_new_drafts_alone() {
        let mut app = crate::tests::test_app();
        app.input.set("first");
        app.queue_while_compacting("first");
        app.input.set("second");
        app.queue_while_compacting("second");
        app.input.set("not submitted");
        assert_eq!(app.compacted_messages.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>(), ["first", "second"]);
        assert_eq!(app.input.text(), "not submitted");
    }
}
