use super::*;

const HELP: &str = "Commands & Skills (Tab to autocomplete):\n\
     • /goal <task>           — autonomous run; its limits live in Settings → Goal\n\
     • /resume                — pick a saved session from this folder and continue it\n\
     • /settings (or Tab)     — open settings configuration tab\n\
     • /context               — show detailed context window breakdown\n\
     • /memory [summary]      — what FlashAgent remembers; the summary groups it by topic\n\
     • /verbose [all|last|off] — toggle verbose mode (or press F2 / Alt+O / Ctrl+O)\n\
     • /effort (or /t)        — choose thinking effort preset (or press F4 / Ctrl+T)\n\
     • /model  (or /m)        — choose and switch model (or press F3)\n\
     • /mode [plan|man|edits|all] — switch permission mode (or press Shift+Tab)\n\
     • /mcp [list|market|test|add|reload] — manage Model Context Protocol servers\n\
     • /sampling              — sampling parameters (or press F5)\n\
     • /compact [focus]       — summarize older turns to free context\n\
     • /clear                 — clear chat scrollback\n\
     • /regenerate (or Ctrl+R) — regenerate last model response from scratch\n\
     • /rewind [n]            — take turns back: files and conversation return to before turn n\n\
     • /diff · /commit <msg>  — git diff --stat / commit staged changes\n\
     • /export [md|html|jsonl] — write the conversation to a file\n\
     • /editor (or Ctrl+E)    — compose the prompt in an external editor\n\
     • Ctrl+V                 — paste a screenshot (Ctrl+Z takes it back); dropping an image works too\n\
     • /update (or Ctrl+U) · /channel <stable|beta> — check, download and install an update\n\
     • /skill:<name>          — invoke a skill from .agents/skills/\n\
     • /exit                  — save the session and quit\n\
     • /uninstall             — close FlashAgent and remove it; asks what data to delete\n\
     • Tab                    — autocomplete popup or settings tab\n\
     • Esc                    — dismiss suggestions / interrupt; twice on an empty prompt quits";

impl App {
    /// The user pressed Enter on a non-empty prompt while no turn was running:
    /// a slash command, a skill, or a message to send.
    pub(crate) async fn submit_input(&mut self, cx: &mut LoopCtx<'_>) -> Flow {
        let line = self.input.trim().to_string();
        if let Some(rest) = line.strip_prefix('/') {
            let (name, arg) = match rest.split_once(char::is_whitespace) {
                Some((name, arg)) => (name, arg.trim()),
                None => (rest, ""),
            };
            if let Some(flow) = self.run_command(cx, name, arg).await {
                return flow;
            }
        }
        self.send_prompt(cx);
        Flow::Next
    }

    /// `/name arg`. `None` when it is not a command after all — a path such as
    /// `/tmp/shot.png` — and the line goes to the model.
    async fn run_command(&mut self, cx: &mut LoopCtx<'_>, name: &str, arg: &str) -> Option<Flow> {
        // A command takes the line out of the composer; a typo leaves it there
        // to fix.
        let typed = self.input.take();
        self.autocomplete_idx = 0;
        match name {
            "help" | "?" => self.chat.push_system(HELP),
            "goal" if arg.is_empty() => self.show_goal_usage(),
            "goal" => self.start_goal(cx, arg),
            "skills" => self.list_skills(),
            "settings" | "config" => {
                let view = self.runtime_settings(cx.perm.state().mode());
                self.open_overlay(Overlay::Settings(Box::new(view)));
            }
            "whatsnew" | "changelog" => self.show_whatsnew(cx).await,
            "memory" | "memories" => {
                let mut modal = open_memory_modal();
                // "/memory summary" opens straight on the overview.
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
                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                self.open_overlay(Overlay::Context(ContextModal::new(self.context_usage.clone())));
            }
            "clear" => self.clear_chat(),
            "regenerate" | "retry" => {
                if !self.regenerate(cx) {
                    self.notice("[No previous turn to regenerate]");
                }
            }
            "effort" | "thinking" | "t" if arg.is_empty() => self.open_effort_menu(cx.source),
            "effort" | "thinking" | "t" => self.set_effort(cx, arg),
            "model" | "models" | "m" => {
                self.open_model_menu(cx.source);
                // "/model qwen" opens the list already narrowed to it.
                if let Some(Overlay::Model(menu)) = self.overlay.as_mut() {
                    arg.chars().for_each(|c| menu.push_filter_char(c));
                }
            }
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
                    // The same as Ctrl+U: check, download and install in one
                    // go, or show the update already under way.
                    self.start_or_watch_update(cx);
                }
            }
            "channel" => self.channel_command(cx, arg),
            "mcp" => self.mcp_command(cx, arg).await,
            "compact" => self.compact_command(cx, arg).await,
            "verbose" | "expand" | "think" | "o" => self.verbose_command(arg),
            "exit" | "quit" | "q" => return Some(Flow::Quit),
            "editor" => match open_in_external_editor("", &self.config.external_editor) {
                Ok(edited) => self.input.set(edited),
                Err(err) => self.notice(format!("Failed to launch external editor: {err}")),
            },
            "diff" => self.git_diff_summary(),
            "rewind" => self.rewind_command(cx, arg),
            "commit" => self.git_commit(arg),
            "export" => self.export_conversation(cx, arg),
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
        let guess = known
            .iter()
            .map(|c| c.trigger.as_str())
            .filter(|t| !t.contains(' '))
            .min_by_key(|t| flashagent_tui::autocomplete::edit_distance(t, &word))
            .filter(|t| flashagent_tui::autocomplete::edit_distance(t, &word) <= 2);
        // The typo stays in the prompt to fix, and the placeholder only shows
        // on an empty one.
        let said = match guess {
            Some(g) => format!("Unknown command {word} · did you mean {g}?"),
            None => format!("Unknown command {word} · /help lists them"),
        };
        self.background = Some(BackgroundNotice::fading(said, 6));
    }

    /// A message for the model: the typed text and any pictures going with it.
    fn send_prompt(&mut self, cx: &LoopCtx<'_>) {
        let text = self.input.take();
        self.remember_prompt(&text);
        self.history_index = None;
        self.current_draft.clear();
        // Sending a new prompt collapses the previous turn's expanded thinking.
        self.last_expanded = false;

        // A path typed out is still the user pointing at a picture — but only
        // a path. Naming a file in a sentence ("open diagram.png and tell
        // me...") is a mention, and the model has view_image for that;
        // silently attaching a megabyte because a word ended in .png would be
        // a surprise.
        //
        // Dropping a file on the terminal is a paste on Unix and plain typing
        // on Windows, where the terminal has no bracketed paste; both end up
        // here, so both behave the same.
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
        // A message that was nothing but the pictures goes as pictures: the
        // path the user dropped is not something to say to the model.
        let text = if left.trim().is_empty() && !self.attachments.is_empty() { String::new() } else { text };
        let shown = if self.attachments.is_empty() {
            text.clone()
        } else {
            // The transcript has to show that a picture went with the
            // message; otherwise the answer refers to something invisible.
            let labels: Vec<String> = self.attachments.iter().map(|a| a.label()).collect();
            if text.trim().is_empty() {
                format!("[{}]", labels.join(", "))
            } else {
                format!("{text}  [{}]", labels.join(", "))
            }
        };
        self.chat.push_user(&shown);
        let content = self.with_memory_if_first(cx, text);
        self.context_before_turn = self.context_usage.total_used();
        let mut user_msg = ChatMessage::user(content);
        if !self.attachments.is_empty() {
            user_msg.images = self.attachments.drain(..).map(|a| a.data_url).collect();
        }
        self.history.push(user_msg);
        self.begin_snapshot_turn(cx);
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
    }

    /// The memory block rides in front of the first user message.
    fn with_memory_if_first(&self, cx: &LoopCtx<'_>, text: String) -> String {
        let first = !self.history.iter().any(|m| m.role == flashagent_llm::Role::User);
        if !first || cx.memory_block.is_empty() {
            text
        } else if text.trim().is_empty() {
            cx.memory_block.to_string()
        } else {
            format!("{}\n\n---\n\n{text}", cx.memory_block)
        }
    }

    /// Mark where /rewind can take the files back to: before the message just
    /// pushed onto the history.
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

        // Save previous state to roll back upon goal completion
        self.goal_state = Some(SavedGoalState {
            mode: cx.perm.state().mode(),
            effort: self.current_effort.clone(),
            max_steps: self.max_steps,
            task: task_desc.clone(),
        });
        self.goal_ledger = Some(GoalLedger::new(task_desc.clone(), goal_budgets.clone()));

        // Lift restrictions for autonomous execution
        cx.perm.state().set_mode(PermissionMode::Bypass);
        cx.perm.state().set_goal_active(true);
        self.current_effort = "high".to_string();
        cx.tools_arc.set_goal_mode(true);

        let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
        let card_w = (w as usize).saturating_sub(4).clamp(44, 110);
        let inner_w = card_w.saturating_sub(2);
        let border_color = "\x1b[38;2;225;175;95m";
        let reset = "\x1b[0m";

        let title = format!(" Goal: {} ", flashagent_tui::truncate_middle(&task_desc, inner_w.saturating_sub(10)));
        let dash_count = inner_w.saturating_sub(visible_width(&title) + 1);
        self.chat.push_line(
            LineKind::System,
            format!("{border_color}╭─\x1b[1;38;2;245;240;232m{title}{border_color}{}╮{reset}", "─".repeat(dash_count)),
        );
        let meta_line = "Mode: Autonomous · Permissions: Auto-Approved · Thinking: Max · Esc to stop";
        let budget_line = flashagent_tui::truncate_middle(&format!("Budget: {}", goal_budgets.summary()), inner_w.saturating_sub(2));
        for row in [meta_line, budget_line.as_str()] {
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
        let content = self.with_memory_if_first(cx, autonomous_directive);
        self.history.push(ChatMessage::user(content));
        self.begin_snapshot_turn(cx);
        self.start_turn(cx, goal_budgets);
    }

    fn list_skills(&mut self) {
        let skills = flashagent_tui::autocomplete::load_skills(std::path::Path::new("."));
        let listed: Vec<String> = skills
            .iter()
            .filter(|s| s.trigger.starts_with("/skill:"))
            .map(|s| format!("  • {} — {}", s.trigger, s.description))
            .collect();
        if listed.is_empty() {
            self.notice("[No skills found. Add .agents/skills/<name>.md (project) or ~/.flashagent/skills/<name>.md (global).]");
        } else {
            self.chat.push_system(&format!("Skills:\n{}", listed.join("\n")));
        }
    }

    fn run_skill(&mut self, cx: &LoopCtx<'_>, name: &str) {
        let name = name.trim();
        let Some(skill_content) = find_skill_file(name).and_then(|p| std::fs::read_to_string(p).ok()) else {
            self.notice(format!("[Unknown skill '{name}'. Type /skills to list available skills.]"));
            return;
        };
        self.chat.push_user(&format!("/skill:{name}"));
        self.history.push(ChatMessage::user(format!("Execute skill: {name}\n\nSkill Instructions:\n{skill_content}")));
        self.begin_snapshot_turn(cx);
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
    }

    async fn show_whatsnew(&mut self, cx: &mut LoopCtx<'_>) {
        let now = flashagent_svc::updater::current_version();
        // Asked for on purpose, so there is no "nothing changed" case worth a
        // blank screen: fall back to the last few releases.
        let mut news = flashagent_tui::whatsnew::since(self.config.last_seen_version.as_deref(), now);
        if news.is_empty() {
            news = flashagent_tui::whatsnew::latest(3);
        }
        if news.is_empty() {
            self.notice("[No changelog is bundled with this build.]");
        } else {
            flashagent_tui::whatsnew::run_channel(news, now, cx.rx).await.ok();
        }
    }

    fn clear_chat(&mut self) {
        self.chat.clear();
        self.renderer.printed_settled = 0;
        self.renderer.prev_expansion = None;
        self.renderer.scroll_to_bottom();
        // Nothing new is printed after a clear, so only a full repaint takes
        // the old lines off the screen.
        self.renderer.request_reprint();
    }

    fn set_effort(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let wanted = arg.to_lowercase();
        let mut known: Vec<String> = ["auto", "default", "off", "low", "medium", "high"].iter().map(|s| s.to_string()).collect();
        if let Some(p) = cx.source.profile() {
            known.extend(p.presets.iter().filter(|p| !known.contains(p)).cloned().collect::<Vec<_>>());
        }
        if !known.contains(&wanted) {
            self.notice(format!("[Unknown effort '{wanted}'. Available: {}]", known.join(", ")));
            return;
        }
        self.current_effort = wanted;
        self.refresh_welcome(cx.source, cx.perm.state().mode(), cx.mascot_mood);
        self.notice(format!("Thinking effort set to: {}", self.current_effort));
    }

    fn mode_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let mode = match arg.to_lowercase().as_str() {
            "" => cx.perm.state().mode().next(),
            "planning" | "plan" => PermissionMode::Planning,
            "manual" | "man" => PermissionMode::Manual,
            "acceptedits" | "accept_edits" | "edits" | "auto" | "default" => PermissionMode::AcceptEdits,
            "bypass" | "accept_all" | "all" => PermissionMode::Bypass,
            "autonomic" => {
                self.notice("[Autonomic mode is temporary and activated exclusively during `/goal <task>` execution]");
                return;
            }
            _ => {
                self.notice("[Usage: /mode planning | /mode manual | /mode edits | /mode all]");
                return;
            }
        };
        cx.perm.state().set_mode(mode);
        self.remember_mode(mode);
        self.refresh_welcome(cx.source, mode, cx.mascot_mood);
        self.notice(format!("Permission mode set to: {}", mode.label()));
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
                self.notice("Invalid channel. Choose either: /channel stable or /channel beta");
                return;
            }
        };
        if wanted == self.config.update_channel {
            self.notice(format!("Already on the {} channel", wanted.label()));
            return;
        }
        // The same yes-or-no card as Settings: switching can replace the
        // binary with an older one, so it is never done on the command alone.
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
                self.notice(format!("Testing MCP server '{rest}'..."));
                // Shown before the wait, which can take a while.
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
                    self.notice(format!("Unknown marketplace extension: '{rest}'. Type /mcp market to see available items."));
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
                    Err(e) => self.notice(format!("\x1b[38;2;245;120;120mFailed to save MCP configuration:\x1b[0m {e}")),
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
                            "\x1b[38;2;135;220;145m✔ MCP reload complete:\x1b[0m {active} active server(s), {tools} discovered tool(s)."
                        ));
                    }
                    Err(e) => self.notice(format!("\x1b[38;2;245;120;120mMCP reload failed:\x1b[0m {e}")),
                }
            }
            _ => self.notice("[Usage: /mcp list | market | test <server> | add <id> | reload]"),
        }
    }

    async fn compact_command(&mut self, cx: &LoopCtx<'_>, focus: &str) {
        self.chat.push_system("Compacting context...");
        self.custom_placeholder = None;
        self.suggested_prompt = None;
        // Shown before the wait: the command that started it is gone from the
        // composer, and its suggestions with it.
        self.draw(cx, None);
        let before = self.context_usage.total_used();
        let focus = (!focus.is_empty()).then_some(focus);
        if compact_context(cx.source, &mut self.history, focus).await.is_some() {
            update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
            let saved = before.saturating_sub(self.context_usage.total_used());
            // Said in the transcript, like the automatic one: it is the
            // conversation that changed.
            self.chat.replace_last_system(&format!(
                "Context compacted · {} saved · the conversation so far is now a summary",
                ContextUsage::format_tokens(saved)
            ));
        } else {
            self.chat.replace_last_system("Nothing to compact yet");
        }
    }

    fn verbose_command(&mut self, arg: &str) {
        (self.last_expanded, self.all_expanded) = match arg {
            // Cycles none -> last -> all -> none, like F2.
            "" => match (self.last_expanded, self.all_expanded) {
                (false, false) => (true, false),
                (true, false) => (false, true),
                _ => (false, false),
            },
            "all" => (false, true),
            "last" => (true, false),
            "off" | "none" | "collapse" => (false, false),
            _ => {
                self.notice("[Usage: /verbose all | /verbose last | /verbose off]");
                return;
            }
        };
    }

    fn git_diff_summary(&mut self) {
        match std::process::Command::new("git").args(["diff", "--stat"]).output() {
            Ok(out) => {
                let s = String::from_utf8_lossy(&out.stdout);
                if !s.trim().is_empty() {
                    self.chat.push_system(&format!("Git diff summary:\n{}", s.trim_end()));
                    return;
                }
                // `git diff` says nothing about files git has never seen, and
                // "clean" next to three untracked files is a lie.
                let untracked = std::process::Command::new("git")
                    .args(["ls-files", "--others", "--exclude-standard"])
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
                    .unwrap_or(0);
                match untracked {
                    0 => self.notice("[No changes to tracked files]"),
                    1 => self.notice("[No changes to tracked files · 1 untracked file]"),
                    n => self.notice(format!("[No changes to tracked files · {n} untracked files]")),
                }
            }
            Err(e) => self.notice(format!("Failed to run git diff: {e}")),
        }
    }

    fn rewind_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        let Some(store) = cx.perm.state().snapshots() else {
            self.notice("[Rewind is not available in this session]");
            return;
        };
        let prompts: Vec<String> =
            self.history.iter().filter(|m| m.role == flashagent_llm::Role::User).map(|m| m.content.clone()).collect();
        let prompt_refs: Vec<&str> = prompts.iter().map(String::as_str).collect();
        let turns = store.rewindable(&prompt_refs);
        if turns.is_empty() {
            self.notice("[Nothing to rewind to yet]");
            return;
        }
        let Some(n) = arg.parse::<usize>().ok().filter(|n| (1..=turns.len()).contains(n)) else {
            if !arg.is_empty() {
                self.notice(format!("[No turn {arg} to rewind to: pick 1 to {}]", turns.len()));
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
            self.notice("[Wait for the turn to finish before rewinding]");
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

    fn export_conversation(&mut self, cx: &LoopCtx<'_>, format_arg: &str) {
        let format_arg = format_arg.to_lowercase();
        // The id already starts with "session_"; the old line produced
        // session_session_1789.md.
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
        match std::fs::write(&filename, content) {
            Ok(_) => {
                let shown = std::fs::canonicalize(&filename).map_or(filename, |p| p.display().to_string());
                self.notice(format!("Exported conversation to: {shown}"));
            }
            Err(e) => self.notice(format!("Failed to export conversation: {e}")),
        }
    }

    /// Takes turn `target` and everything after it back: the files it
    /// changed return to how they were, and its prompt goes back in the
    /// input. Only reached after the confirmation card the user was shown
    /// answered "yes" — `/rewind <n>` itself only opens that card.
    pub(crate) fn commit_rewind(&mut self, cx: &mut LoopCtx<'_>, target: flashagent_core::Rewindable) {
        let Some(store) = cx.perm.state().snapshots() else {
            return;
        };
        let prompts: Vec<String> = self
            .history
            .iter()
            .filter(|m| m.role == flashagent_llm::Role::User)
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
            self.notice("Rewind incomplete. History and failed snapshots kept; fix the reported errors and retry /rewind.".to_string());
            self.renderer.request_reprint();
            return;
        }
        let taken_back = prompts.len() - target.user_index;
        let cut = self
            .history
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == flashagent_llm::Role::User)
            .nth(target.user_index)
            .map(|(i, _)| i);
        if let Some(cut) = cut {
            self.history.truncate(cut);
        }
        self.chat.truncate_before_nth_last_user(taken_back);
        self.renderer.scroll_to_bottom();
        self.renderer.printed_settled = 0;
        self.renderer.prev_expansion = None;
        update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
        self.suggested_prompt = None;
        self.latest_suggestion = None;
        self.custom_placeholder = None;

        // The words, not the scaffolding a goal or the first message's
        // memory block wrapped them in.
        self.input.set(extract_user_prompt(&prompt).to_string());
        self.autosave(cx.session_id, cx.cwd_display);
        self.renderer.request_reprint();
    }
}
