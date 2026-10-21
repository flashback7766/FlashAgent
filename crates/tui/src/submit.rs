use super::*;
use flashagent_tui::i18n::{tr, tr_fill};

/// Messages typed while a compaction owns the history. The history is with the
/// task (`std::mem::take` in `compact_command`), so a turn started now would
/// begin from nothing: the words wait here and go out together once the summary
/// is back. A static rather than an `App` field because `App` lives in main.rs,
/// which must not change for this.
static COMPACT_QUEUE: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// Typed while the context was being compacted: it waits for the summary.
fn queue_compacted_message(text: String) {
    let mut queue = COMPACT_QUEUE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    queue.push(text);
}

/// Everything typed during the compaction, oldest first. Empty when nothing
/// waited.
pub(crate) fn take_compacted_messages() -> Vec<String> {
    let mut queue = COMPACT_QUEUE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    std::mem::take(&mut *queue)
}

const HELP_COMMANDS: &[(&str, &str)] = &[
    ("/goal <task>", "an autonomous run; its limits are in Settings \u{2192} Goal"),
    ("/resume", "pick a saved session from this folder and continue it"),
    ("/settings, Tab", "settings"),
    ("/model, /m, F3", "choose and switch the model"),
    ("/provider [name]", "switch to another saved provider, local or cloud; add or edit them"),
    ("/effort, /t, F4", "thinking effort (also Ctrl+T)"),
    ("/mode [plan|man|edits|all]", "permission mode (also Shift+Tab)"),
    ("/context, F1", "what fills the context window"),
    ("/compact [focus]", "summarize older turns to free context"),
    ("/memory [summary]", "what FlashAgent remembers; the summary groups it by topic"),
    ("/verbose [all|last|off]", "show thoughts and tool calls (also F2, Alt+O, Ctrl+O)"),
    ("/regenerate, Ctrl+R", "answer the last prompt again from scratch"),
    ("/rewind [n]", "files and conversation go back to before turn n"),
    ("/diff, /commit <msg>", "the changed lines and untracked files; commit the staged changes"),
    ("/export [md|html|jsonl] [path]", "write the conversation to a file, where you say"),
    ("/editor, Ctrl+X Ctrl+E", "write the prompt in an external editor"),
    ("/tools", "what the model may call, and what it may not"),
    ("/notices", "what the app said, kept so it can be read again"),
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
    const KEY: &str = "\x1b[38;2;194;231;255m";
    const WHAT: &str = "\x1b[38;2;165;160;150m";
    const OFF: &str = "\x1b[0m";
    let width = width.max(30);
    let key_w = (width / 3).clamp(12, 26);
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
fn split_command(line: &str) -> (&str, &str) {
    let rest = line.strip_prefix('/').unwrap_or(line);
    match rest.split_once(char::is_whitespace) {
        Some((name, arg)) => (name, arg.trim()),
        None => (rest, ""),
    }
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
        "help" | "?" | "whatsnew" | "changelog" | "memory" | "memories" | "context" | "verbose" | "expand" | "think"
        | "o" | "diff" | "commit" | "settings" | "config" | "sampling" | "params" | "effort" | "thinking" | "t"
        | "model" | "models" | "m" | "mode" | "skills" | "tools" | "notices" | "log" | "safe" => WhileRunning::Now,
        "compact" | "clear" | "export" | "rewind" => WhileRunning::AfterTurn,
        "goal" | "resume" | "regenerate" | "retry" | "update" | "channel" | "mcp" | "editor" | "exit" | "quit" | "q"
        | "uninstall" => WhileRunning::Refuse,
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
        rows.push(format!("  \u{2026} and {} more", files.len() - 8));
    }
    rows.join("\n")
}

impl App {
    /// Enter on a non-empty prompt with no turn running.
    pub(crate) async fn submit_input(&mut self, cx: &mut LoopCtx<'_>) -> Flow {
        let line = self.input.trim().to_string();
        if let Some(rest) = line.strip_prefix('/') {
            let (name, arg) = match rest.split_once(char::is_whitespace) {
                Some((name, arg)) => (name, arg.trim()),
                None => (rest, ""),
            };
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
        if !text.is_empty() {
            queue_compacted_message(text);
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
        let queued = take_compacted_messages();
        if queued.is_empty() {
            return;
        }
        let joined = queued.join("\n\n");
        if self.running {
            if let Some(tx) = self.active_steer_tx.clone() {
                let _ = tx.send(flashagent_core::Steer::text(joined.clone()));
                self.pending_steers.push((joined, Vec::new()));
            } else {
                // No channel: the words go back to the prompt rather than away.
                let draft = self.input.take();
                self.input.set(if draft.trim().is_empty() { joined } else { format!("{joined}\n\n{draft}") });
            }
            self.renderer.request_reprint();
            return;
        }
        let draft = self.input.take();
        self.input.set(if draft.trim().is_empty() { joined } else { format!("{joined}\n\n{draft}") });
        self.send_prompt(cx);
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
                    self.say(BackgroundNotice::fading(format!("/{name} will run when this turn finishes \u{b7} Esc ends the turn now instead"), 6).warning());
                }
            }
            WhileRunning::Refuse => self.say(BackgroundNotice::fading(format!("/{name} does not run while the model works \u{b7} Esc interrupts the turn first"), 6).warning()),
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
            "cost" => self.show_cost(),
            "whatsnew" | "changelog" => self.show_whatsnew(cx).await,
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
                update_context_usage(&self.current_model, &mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
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
                    Err(err) => self.notice(format!("Failed to launch external editor: {err}")),
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
            Some(g) => format!("Unknown command {word} · did you mean {g}?"),
            None => format!("Unknown command {word} · /help lists them"),
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
    pub(crate) fn flush_switch_notice(&mut self) {
        let pending = std::mem::take(&mut self.pending_switch);
        let Some(label) = pending.label(&self.current_model, &self.current_effort) else { return };
        if self.chat.push_model_notice(&label) {
            self.renderer.request_reprint();
        }
    }

    fn send_prompt(&mut self, cx: &LoopCtx<'_>) {
        let text = self.input.take();
        self.remember_prompt(&text);
        self.history_index = None;
        self.current_draft.clear();
        self.last_expanded = false;

        // Only a path typed out on its own attaches a picture; a file named in a
        // sentence is a mention, and the model has view_image for it. A dropped file
        // arrives as a paste on Unix and as typing on Windows; both land here.
        let mut left = String::new();
        for token in text.split_whitespace() {
            let attached = (token.contains('/') || token.contains('\\'))
                && self.attachments.len() < 8
                && match Attachment::from_dropped_path(token) {
                    Some(att) => {
                        if !self.attachments.iter().any(|a| a.data_url == att.data_url) {
                            self.attachments.push(att);
                        }
                        true
                    }
                    None => false,
                };
            if !attached {
                if !left.is_empty() {
                    left.push(' ');
                }
                left.push_str(token);
            }
        }
        // A message of only pictures: the dropped path is not text for the model.
        let text = if left.trim().is_empty() && !self.attachments.is_empty() { String::new() } else { text };
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
        self.flush_switch_notice();
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
        self.chat.push_system(&format!(
            "Autonomous Goal Mode:\n\
             Usage: /goal <task description>\n\
             Example: /goal Refactor error handling in the core crate and run the test suite\n\
             Limits: {} — change them in Settings → Goal.\n\
             In goal mode FlashAgent approves its own tool calls (dangerous shell commands are refused), \
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

        // Restored when the goal ends.
        self.goal_state = Some(SavedGoalState {
            mode: cx.perm.state().mode(),
            effort: self.current_effort.clone(),
            max_steps: self.max_steps,
            task: task_desc.clone(),
        });
        self.goal_ledger = Some(GoalLedger::new(task_desc.clone(), goal_budgets.clone()));

        cx.perm.state().set_mode(PermissionMode::Bypass);
        cx.perm.state().set_goal_active(true);
        self.current_effort = "high".to_string();
        cx.tools_arc.set_goal_mode(true);
        self.goal_active = true;
        self.apply_personality();
        self.refresh_welcome_state(cx.source, cx.mascot_mood);
        // A switch made before the goal belongs above the goal card, not inside it.
        self.flush_switch_notice();

        let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
        let card_w = (w as usize).saturating_sub(4).clamp(20, 110);
        let inner_w = card_w.saturating_sub(2);
        let border_color = "\x1b[38;2;225;175;95m";
        let reset = "\x1b[0m";

        let title = format!(" Goal: {} ", flashagent_tui::truncate_middle(&task_desc, inner_w.saturating_sub(10)));
        let dash_count = inner_w.saturating_sub(visible_width(&title) + 1);
        self.chat.push_line(
            LineKind::System,
            format!("{border_color}╭─\x1b[1;38;2;245;240;232m{title}{border_color}{}╮{reset}", "─".repeat(dash_count)),
        );
        let meta = "Accept All \u{b7} thinking high \u{b7} Esc stops the run";
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
            "This run has no budget: it ends when you finish or the user stops it. Say plainly \
             what is left unfinished or unverified rather than claiming success."
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
             5. Call update_plan with your whole step-by-step plan before starting, \
             and again whenever a step finishes or the plan changes — the person \
             who started this run watches it live and has no other way to see where \
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
                Some(near) => format!("Unknown skill '{name}' · did you mean '{near}'?"),
                None => format!("Unknown skill '{name}' · /skills lists them"),
            };
            self.notice(said);
            return;
        };
        self.flush_switch_notice();
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

    /// `/cost`: what the session has cost, and where. Says plainly when the
    /// backend prices nothing, rather than reporting a confident zero.
    fn show_cost(&mut self) {
        use flashagent_core::CostLedger;
        let Some(total) = self.cost.total() else {
            self.chat.push_system(&format!(
                "No backend in this session reported a price \u{b7} {} is running free on your own hardware, or its API does not say what a turn costs",
                self.config.active_profile().name
            ));
            return;
        };
        self.chat.push_system(&format!(
            "This session: ${} over {}",
            CostLedger::format_amount(total),
            CostLedger::turns_phrase(self.cost.priced_turns())
        ));
        // What the providers behind the gateways charged: the difference is
        // what the gateway added for itself.
        if let Some(upstream) = self.cost.upstream_total() {
            self.chat.push_system(&format!(
                "  the providers themselves: ${} over {}",
                CostLedger::format_amount(upstream),
                CostLedger::turns_phrase(self.cost.upstream_turns())
            ));
        }
        for row in self.cost.by_provider() {
            // A model the backend prices at nothing has no bill to break down:
            // saying "$0.00 over 12 turns" reads as an empty account rather than
            // as one that does not exist.
            let line = if row.total == 0.0 {
                format!("  {} \u{b7} free", row.name)
            } else {
                format!(
                    "  {} \u{b7} ${} over {}",
                    row.name,
                    CostLedger::format_amount(row.total),
                    CostLedger::turns_phrase(row.turns)
                )
            };
            self.chat.push_system(&line);
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
            "" => cx.perm.state().mode().next(),
            "planning" | "plan" => PermissionMode::Planning,
            "manual" | "man" => PermissionMode::Manual,
            "acceptedits" | "accept_edits" | "edits" | "auto" | "default" => PermissionMode::AcceptEdits,
            "bypass" | "accept_all" | "all" => PermissionMode::Bypass,
            "autonomic" => {
                self.notice(tr("Autonomous mode runs only inside /goal <task>"));
                return;
            }
            _ => {
                self.notice(tr("Usage: /mode planning | /mode manual | /mode edits | /mode all"));
                return;
            }
        };
        cx.perm.state().set_mode(mode);
        self.remember_mode(mode);
        self.refresh_welcome(cx.source, cx.mascot_mood);
        self.notice(format!("{} {}", tr("Permission mode set to:"), tr(mode.label())));
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
            "test" if rest.is_empty() => self.chat.push_system("Usage: /mcp test <server_name>\nExample: /mcp test sqlite"),
            "test" => {
                self.notice(format!("Testing MCP server '{rest}'…"));
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
                        self.chat.push_system(&format!("\x1b[38;2;245;120;120mMCP server '{rest}' test failed:\x1b[0m\n{e}"));
                        self.custom_placeholder = None;
                    }
                }
            }
            "add" if rest.is_empty() => self.chat.push_system(
                "Usage: /mcp add <marketplace_id>\nExample: /mcp add sqlite\nType /mcp market to browse extensions.",
            ),
            "add" => {
                let Some(item) = flashagent_tools::mcp::find_marketplace_item(rest) else {
                    self.notice(format!("Unknown extension '{rest}' · /mcp market lists them"));
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
                    Err(e) => self.notice(format!("Failed to save the MCP configuration: {e}")),
                }
            }
            "reload" => {
                let mgr = cx.tools_arc.mcp_manager();
                match mgr.reload().await {
                    Ok(()) => {
                        let statuses = mgr.server_status_list().await;
                        let active = statuses.iter().filter(|s| s.state == flashagent_tools::mcp::ServerConnectionState::Active).count();
                        let tools: usize = statuses.iter().map(|s| s.tool_count).sum();
                        self.notice(format!(
                            "MCP reloaded · {} · {}",
                            flashagent_tui::plural(active, "active server", "active servers"),
                            flashagent_tui::plural(tools, "tool", "tools")
                        ));
                    }
                    Err(e) => self.notice(format!("MCP reload failed: {e}")),
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

        // The task owns the history until it is done; there is nothing to send
        // a turn with in the meantime, and a turn would have nothing to keep.
        let mut history = std::mem::take(&mut self.history);
        // What the window holds now: the number the summary is measured
        // against, and what the card says it freed.
        let used = self.context_usage.total_used();
        // The room one request may carry: the window minus the prompt, the
        // memory and the schemas — once. What the conversation holds now is
        // what gets summarized, so it is not subtracted here.
        let room = crate::compact::room_for(
            self.context_usage.total_capacity,
            self.context_usage.system_tokens + self.context_usage.memory_tokens + self.context_usage.tools_tokens,
        );
        let source = cx.source.clone();
        let focus = (!focus.is_empty()).then(|| focus.to_string());
        let tx = cx.tx.clone();
        tokio::spawn(async move {
            let sink = SummarySink::ui(tx.clone());
            let result =
                compact_context_with_sink(
                source.as_ref(),
                &mut history,
                crate::compact::Compaction {
                    focus_prompt: focus.as_deref(),
                    archive_path: &archive,
                    real_used: Some(used),
                    room: Some(room),
                    allow_growth: true,
                },
                Some(&sink),
            )
            .await;
            let saved = result.as_ref().map(|r| r.saved);
            let _ = tx.send(UiEvent::Compacted { history, saved, card: result });
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
                self.notice(format!("Failed to run git diff: {e}"));
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
                Some(b'-') => "\x1b[38;2;225;115;105m",
                Some(b'@') => "\x1b[38;2;135;130;125m",
                _ => "\x1b[38;2;165;160;150m",
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
                "Turns you can go back to. The files FlashAgent changed and the conversation return to how they were before the turn:\n",
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
            text.push_str("Type /rewind <number>. Changes made by shell commands are not undone.");
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
                self.chat.push_system(&format!("Git commit succeeded:\n{}", String::from_utf8_lossy(&out.stdout).trim_end()));
            }
            Ok(out) => {
                let err = String::from_utf8_lossy(&out.stderr);
                let s = String::from_utf8_lossy(&out.stdout);
                self.chat.push_system(&format!("Git commit status:\n{s}{err}"));
            }
            Err(e) => self.notice(format!("Failed to run git commit: {e}")),
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
                format!("[{} attached image{}]", m.images.len(), if m.images.len() == 1 { "" } else { "s" })
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
                self.say(BackgroundNotice::fading(format!("Exported to {shown}"), 8));
                self.renderer.request_reprint();
            }
            Err(e) => self.notice(format!("Failed to export conversation: {e}")),
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
                self.chat.push_line(LineKind::ToolError, format!("Not put back: {} — {why}", store.display_path(path)));
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
        update_context_usage(&self.current_model, &mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
        self.suggested_prompt = None;
        self.latest_suggestion = None;
        self.custom_placeholder = None;

        // The words, without goal scaffolding or the memory block.
        self.input.set(extract_user_prompt(&prompt).to_string());
        self.autosave(cx.session_id, cx.cwd_display);
        self.renderer.request_reprint();
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
        for name in ["help", "verbose", "diff", "mode", "effort", "model", "memory", "context"] {
            assert_eq!(while_running(name), WhileRunning::Now, "{name}");
        }
        // The conversation belongs to the turn until it ends.
        for name in ["compact", "clear", "export", "rewind"] {
            assert_eq!(while_running(name), WhileRunning::AfterTurn, "{name}");
        }
        for name in ["goal", "resume", "regenerate", "exit", "uninstall", "skill:review", "update"] {
            assert_eq!(while_running(name), WhileRunning::Refuse, "{name}");
        }
        // A path is for the model; a misspelt command is not.
        assert_eq!(while_running("tmp/shot.png"), WhileRunning::Text);
        assert_eq!(while_running(""), WhileRunning::Text);
        assert_eq!(while_running("hlep"), WhileRunning::Unknown);
    }

    #[test]
    fn a_command_line_splits_into_name_and_argument() {
        assert_eq!(split_command("/compact keep the schema"), ("compact", "keep the schema"));
        assert_eq!(split_command("/help"), ("help", ""));
        assert_eq!(split_command("/tmp/shot.png"), ("tmp/shot.png", ""));
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
    fn messages_queued_during_compaction_keep_their_order() {
        assert!(take_compacted_messages().is_empty());
        queue_compacted_message("first".into());
        queue_compacted_message("second".into());
        assert_eq!(take_compacted_messages(), ["first", "second"]);
        assert!(take_compacted_messages().is_empty());
    }
}
