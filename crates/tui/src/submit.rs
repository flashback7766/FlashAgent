use super::*;

impl App {
    /// The user pressed Enter on a non-empty prompt while no turn was running:
    /// a slash command, a skill, or a message to send.
    pub(crate) async fn submit_input(&mut self, cx: &mut LoopCtx<'_>) -> Flow {
        macro_rules! notice {
            ($text:expr) => {{
                self.custom_placeholder = Some(($text).to_string());
                self.suggested_prompt.take();
                self.renderer.request_reprint();
            }};
        }

                    let trimmed = self.input.trim();
                    if trimmed == "/help" || trimmed == "/?" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.push_system(
                            "Commands & Skills (Tab to autocomplete):\n\
                             • /goal [--steps N] [--time 30m] [--tokens 200k] <task> — autonomous run under a budget\n\
                             • /settings (or Tab)     — open settings configuration tab\n\
                             • /context               — show detailed context window breakdown\n\
                             • /verbose [all|last|off] — toggle verbose mode (or press F2 / Alt+O / Ctrl+O)\n\
                             • /effort (or /t)        — choose thinking effort preset (or press F4 / Ctrl+T)\n\
                             • /model  (or /m)        — choose and switch model (or press F3)\n\
                             • /mode [plan|man|edits|all] — switch permission mode (or press Shift+Tab)\n\
                             • /mcp [list|market|test|add|reload] — manage Model Context Protocol servers\n\
                             • /sampling              — sampling parameters (or press F5)\n\
                             • /compact [focus]       — summarize older turns to free context\n\
                             • /clear                 — clear chat scrollback\n\
                             • /regenerate (or Ctrl+R) — regenerate last model response from scratch\n\
                             • /diff · /commit <msg>  — git diff --stat / commit staged changes\n\
                             • /export [md|html|jsonl] — write the conversation to a file\n\
                             • /editor (or Ctrl+E)    — compose the prompt in an external editor\n\
                             • Ctrl+V                 — paste a screenshot for a vision model (Ctrl+Z takes it back); dropping an image file works too\n\
                             • /update (or Ctrl+U) · /channel <stable|beta> — check, download and install an update, with progress\n\
                             • /skill:<name>          — invoke a skill from .agents/skills/\n\
                             • /exit                  — save the session and quit\n\
                             • Tab                    — autocomplete popup or settings tab\n\
                             • Esc                    — dismiss suggestions / interrupt; quits on an empty prompt"
                        );
                        return Flow::Continue;
                    }

                    if trimmed == "/goal" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.push_system(&format!(
                            "Autonomous Goal Mode:\n\
                             Usage: /goal [--steps N] [--time 30m] [--tokens 200k] <task description>\n\
                             Example: /goal --time 20m Refactor error handling in core crate and run test suite\n\
                             Budgets default to {} and stop the run when reached; --tokens counts generated tokens only.\n\
                             In goal mode, FlashAgent lifts all permission gates, maximizes reasoning, and executes autonomously without human interruption until a budget or the task ends it.",
                            GoalBudgets::default().summary()
                        ));
                        return Flow::Continue;
                    } else if let Some(task) = trimmed.strip_prefix("/goal ") {
                        let task = task.to_string();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let (goal_budgets, task_desc) =
                            match flashagent_tui::goal::parse_goal_command(&task) {
                                Ok(parsed) => parsed,
                                Err(msg) => {
                                    notice!(&msg);
                                    self.renderer.request_reprint();
                                    return Flow::Continue;
                                }
                            };

                        // Save previous state to roll back upon goal completion
                        self.goal_state = Some(SavedGoalState {
                            mode: cx.perm.state().mode(),
                            effort: self.current_effort.clone(),
                            max_steps: self.max_steps,
                            task: task_desc.clone(),
                        });
                        self.goal_ledger =
                            Some(GoalLedger::new(task_desc.clone(), goal_budgets.clone()));

                        // Lift restrictions for autonomous execution
                        cx.perm.state().set_mode(PermissionMode::Bypass);
                        self.current_effort = "high".to_string();
                        cx.tools_arc.set_goal_mode(true);

                        let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                        let card_w = (w as usize).saturating_sub(4).clamp(44, 110);
                        let inner_w = card_w.saturating_sub(2);
                        let border_color = "\x1b[38;2;225;175;95m";
                        let reset = "\x1b[0m";

                        let title = format!(" Goal: {} ", flashagent_tui::truncate_middle(&task_desc, inner_w.saturating_sub(10)));
                        let dash_count = inner_w.saturating_sub(title.chars().count() + 1);
                        self.chat.push_line(LineKind::System, format!("{border_color}╭─\x1b[1;38;2;245;240;232m{title}{border_color}{}╮{reset}", "─".repeat(dash_count)));
                        let meta_line = "Mode: Autonomous · Permissions: Auto-Approved · Thinking: Max · Esc to stop";
                        let pad_len = inner_w.saturating_sub(meta_line.chars().count() + 1);
                        self.chat.push_line(LineKind::System, format!("{border_color}│{reset} \x1b[38;2;160;155;145m{meta_line}\x1b[0m{border_color}{}│{reset}", " ".repeat(pad_len)));
                        let budget_line = format!("Budget: {}", goal_budgets.summary());
                        let budget_line = flashagent_tui::truncate_middle(&budget_line, inner_w.saturating_sub(2));
                        let pad_len = inner_w.saturating_sub(budget_line.chars().count() + 1);
                        self.chat.push_line(LineKind::System, format!("{border_color}│{reset} \x1b[38;2;160;155;145m{budget_line}\x1b[0m{border_color}{}│{reset}", " ".repeat(pad_len)));
                        self.chat.push_line(LineKind::System, format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w)));

                        self.chat.push_user(&format!("/goal {task_desc}"));

                        let autonomous_directive = format!(
                            "[AUTONOMOUS GOAL DIRECTIVE]\n\
                             You are operating in fully autonomous /goal mode.\n\
                             Target goal: {}\n\n\
                             Autonomous Rules:\n\
                             1. Do NOT ask clarifying questions or seek user confirmation. All tool actions are pre-approved.\n\
                             2. Plan, research, edit, execute, and verify completely on your own.\n\
                             3. Thoroughly test and verify your changes before finishing.\n\
                             4. Conclude with a clear structured summary of what was accomplished.\n\n\
                             Budget for this run: {}. When it runs out the run is stopped wherever it \
                             is, so do the load-bearing work first and say plainly what is left \
                             unfinished or unverified rather than claiming success.",
                            task_desc,
                            goal_budgets.summary()
                        );

                        let first = !self.history.iter().any(|m| m.role == flashagent_llm::Role::User);
                        let content = if first && !cx.memory_block.is_empty() {
                            format!("{}\n\n---\n\n{autonomous_directive}", cx.memory_block)
                        } else {
                            autonomous_directive
                        };
                        self.history.push(ChatMessage::user(content));
                        update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                        cx.cancel.store(false, Ordering::Relaxed);
                        self.suggested_prompt = None;
                        self.custom_placeholder = None;
                        self.running = true;
                        self.turn_phase = TurnPhase::Waiting;
                        self.turn_started = Some(std::time::Instant::now());
                        self.token_tracker.on_turn_start(self.current_model.clone(), self.context_usage.total_used());
                        cx.source.set_model(&self.current_model);
                        cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                        cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                        let turn_opts = build_turn_options(&self.config, &self.current_effort);
                        self.turn_counter += 1;
                        self.turn_outcome = flashagent_core::TurnOutcome::default();
                        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                        self.active_steer_tx = Some(steer_tx);
                        self.active_turn_handle = Some(spawn_turn(
                            cx.cancel.clone(),
                            cx.source.clone(),
                            cx.perm,
                            self.history.clone(),
                            goal_budgets,
                            turn_opts,
                            cx.tx.clone(),
                            steer_rx,
                            self.turn_counter,
                        ));
                        return Flow::Continue;
                    }

                    if trimmed == "/skills" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let skills = flashagent_tui::autocomplete::load_skills(std::path::Path::new("."));
                        let listed: Vec<String> = skills
                            .iter()
                            .filter(|s| s.trigger.starts_with("/skill:"))
                            .map(|s| format!("  • {} — {}", s.trigger, s.description))
                            .collect();
                        if listed.is_empty() {
                            notice!("[No skills found. Add .agents/skills/<name>.md (project) or ~/.flashagent/skills/<name>.md (global).]");
                        } else {
                            self.chat.push_system(&format!("Skills:\n{}", listed.join("\n")));
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/settings" || trimmed == "/config" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let runtime_mode = self.goal_state.as_ref().map_or(cx.perm.state().mode(), |g| g.mode);
                        let effort = self.goal_state.as_ref().map_or(self.current_effort.as_str(), |g| g.effort.as_str());
                        self.settings_view = Some(settings_for_runtime(&self.config, runtime_mode, effort, &self.current_model, &self.available_models, self.context_usage.total_capacity));
                        return Flow::Continue;
                    }

                    if trimmed == "/whatsnew" || trimmed == "/changelog" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let now = flashagent_svc::updater::current_version();
                        // Asked for on purpose, so there is no
                        // "nothing changed" case worth a blank
                        // screen: fall back to the last few releases.
                        let mut news = flashagent_tui::whatsnew::since(
                            self.config.last_seen_version.as_deref(),
                            now,
                        );
                        if news.is_empty() {
                            news = flashagent_tui::whatsnew::latest(3);
                        }
                        if news.is_empty() {
                            notice!("[No changelog is bundled with this build.]");
                        } else {
                            flashagent_tui::whatsnew::run_channel(news, now, cx.rx)
                                .await
                                .ok();
                            self.renderer.request_reprint();
                        }
                        return Flow::Continue;
                    }

                    if trimmed == "/memory" || trimmed == "/memories" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.memory_modal = Some(flashagent_tui::memory_view::MemoryModal::new(&std::env::current_dir().unwrap_or_default()));
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/context" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                        self.context_modal = Some(ContextModal::new(self.context_usage.clone()));
                        return Flow::Continue;
                    }

                    if trimmed == "/clear" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.clear();
                        self.renderer.printed_settled = 0;
                        self.renderer.prev_expansion = None;
                        return Flow::Continue;
                    }

                    if trimmed == "/regenerate" || trimmed == "/retry" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        if let Some(user_idx) = self.history.iter().rposition(|m| m.role == flashagent_llm::Role::User) {
                            let steps = self.effort_memory.observe(
                                &self.current_model,
                                &flashagent_core::TurnOutcome { regenerated: true, ..Default::default() },
                            );
                            self.effort_memory.save();
                            cx.source.set_effort_bias(steps);
                            self.history.truncate(user_idx + 1);
                            self.chat.truncate_to_last_user();
                            self.renderer.scroll_to_bottom();
                            self.renderer.printed_settled = 0;
                            self.renderer.prev_expansion = None;
                            self.renderer.request_reprint();
                            update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                            cx.cancel.store(false, Ordering::Relaxed);
                            self.suggested_prompt = None;
                            self.custom_placeholder = None;
                            self.last_expanded = false;
                            self.running = true;
                        self.turn_phase = TurnPhase::Waiting;
                            self.turn_started = Some(std::time::Instant::now());
                            self.token_tracker.on_turn_start(self.current_model.clone(), self.context_usage.total_used());
                            cx.source.set_model(&self.current_model);
                            cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                            cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                            let turn_opts = build_turn_options(&self.config, &self.current_effort);
                            self.turn_counter += 1;
                        self.turn_outcome = flashagent_core::TurnOutcome::default();
                            let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                            self.active_steer_tx = Some(steer_tx);
                            self.active_turn_handle = Some(spawn_turn(
                                cx.cancel.clone(),
                                cx.source.clone(),
                                cx.perm,
                                self.history.clone(),
                                GoalBudgets::steps_only(self.max_steps),
                                turn_opts,
                                cx.tx.clone(),
                                steer_rx,
                                self.turn_counter,
                            ));
                        } else {
                            notice!("[No previous turn to regenerate]");
                            self.renderer.request_reprint();
                        }
                        return Flow::Continue;
                    }

                    if trimmed == "/effort" || trimmed == "/thinking" || trimmed == "/t" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let mut menu = build_effort_menu(cx.source, &self.effort_memory, &self.current_model);
                        menu.select_by_value(&self.current_effort);
                        self.effort_menu = Some(menu);
                        return Flow::Continue;
                    } else if let Some(arg) = trimmed.strip_prefix("/effort ") {
                        let arg_val = arg.trim().to_lowercase();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let mut known: Vec<String> = ["auto", "default", "off", "low", "medium", "high"].iter().map(|s| s.to_string()).collect();
                        if let Some(p) = cx.source.profile() {
                            known.extend(p.presets.iter().cloned());
                        }
                        if !known.contains(&arg_val) {
                            known.dedup();
                            notice!(&format!("[Unknown effort '{arg_val}'. Available: {}]", known.join(", ")));
                            self.renderer.request_reprint();
                            return Flow::Continue;
                        }
                        self.current_effort = arg_val.clone();
                        refresh_welcome_card_if_before_user_msg(
                            &mut self.chat,
                            &mut self.renderer,
                            &self.current_model,
                            cx.cwd_display,
                            cx.perm.state().mode().label(),
                            cx.memory_docs,
                            cx.source,
                            &self.current_effort,
                            self.current_context.as_deref(),
                            self.config.show_mascot,
                            cx.mascot_mood,
                        );
                        self.custom_placeholder = Some(format!("Thinking effort set to: {arg_val}"));
                        self.suggested_prompt = None;
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/model" || trimmed == "/models" || trimmed == "/m" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        if let Some(mut menu) = build_model_menu(cx.source) {
                            menu.select_by_value(&self.current_model);
                            self.model_menu = Some(menu);
                        } else {
                            notice!("[No models discovered from server]");
                        }
                        return Flow::Continue;
                    }

                    if trimmed == "/sampling" || trimmed == "/params" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.sampling_view = Some(SamplingView::new(&self.config));
                        return Flow::Continue;
                    }

                    if trimmed == "/mode" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let next_mode = cx.perm.state().mode().next();
                        cx.perm.state().set_mode(next_mode);
                        refresh_welcome_card_if_before_user_msg(
                            &mut self.chat,
                            &mut self.renderer,
                            &self.current_model,
                            cx.cwd_display,
                            next_mode.label(),
                            cx.memory_docs,
                            cx.source,
                            &self.current_effort,
                            self.current_context.as_deref(),
                            self.config.show_mascot,
                            cx.mascot_mood,
                        );
                        self.custom_placeholder = Some(format!("Permission mode set to: {}", next_mode.label()));
                        self.suggested_prompt = None;
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    } else if let Some(arg) = trimmed.strip_prefix("/mode ") {
                        let arg_val = arg.trim().to_lowercase();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let m = match arg_val.as_str() {
                            "planning" | "plan" => Some(PermissionMode::Planning),
                            "manual" | "man" => Some(PermissionMode::Manual),
                            "acceptedits" | "accept_edits" | "edits" | "auto" | "default" => Some(PermissionMode::AcceptEdits),
                            "bypass" | "accept_all" | "all" => Some(PermissionMode::Bypass),
                            "autonomic" => {
                                notice!("[Autonomic mode is temporary and activated exclusively during `/goal <task>` execution]");
                                None
                            }
                            _ => {
                                notice!("[Usage: /mode planning | /mode manual | /mode edits | /mode all]");
                                None
                            }
                        };
                        if let Some(mode) = m {
                            cx.perm.state().set_mode(mode);
                            refresh_welcome_card_if_before_user_msg(
                                &mut self.chat,
                                &mut self.renderer,
                                &self.current_model,
                                cx.cwd_display,
                                mode.label(),
                                cx.memory_docs,
                                cx.source,
                                &self.current_effort,
                                self.current_context.as_deref(),
                                self.config.show_mascot,
                                cx.mascot_mood,
                            );
                            self.custom_placeholder = Some(format!("Permission mode set to: {}", mode.label()));
                            self.suggested_prompt = None;
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/update" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        if flashagent_svc::updater::is_dev_mode() {
                            self.chat.push_system("  \x1b[38;2;225;175;95mAuto-updater is disabled in dev mode\x1b[0m (running from source repository / cargo build).\n  To update your dev build, pull latest git commits and run `cargo build --release`.");
                            return Flow::Continue;
                        }
                        let channel = self.config.update_channel;
                        notice!(&format!("Checking for updates on {} channel...", channel.label()));
                        let update_tx_clone = cx.update_tx.clone();
                        tokio::spawn(async move {
                            match flashagent_svc::updater::check_for_updates(channel, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                                Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                    let _ = update_tx_clone.send(UpdateNotice::Available {
                                        version: target,
                                        asset_name,
                                        download_url,
                                        checksums_url,
                                    });
                                }
                                Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => {
                                    let _ = update_tx_clone.send(UpdateNotice::UpToDate {
                                        version: current,
                                    });
                                }
                                Err(e) => {
                                    let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                                }
                            }
                        });
                        return Flow::Continue;
                    }

                    if trimmed == "/channel" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.push_system(&format!(
                            "Current release channel: \x1b[1m{}\x1b[0m\nUsage: /channel <stable|beta>\n• /channel stable — Official stable releases (v*)\n• /channel beta   — Latest beta pre-releases (b*)",
                            self.config.update_channel.label()
                        ));
                        return Flow::Continue;
                    } else if let Some(arg) = trimmed.strip_prefix("/channel ") {
                        let choice = arg.trim().to_lowercase();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let target_ch = match choice.as_str() {
                            "stable" | "v" => Some(flashagent_core::config::UpdateChannel::Stable),
                            "beta" | "b" => Some(flashagent_core::config::UpdateChannel::Beta),
                            _ => None,
                        };
                        if let Some(ch) = target_ch {
                            self.config.update_channel = ch;
                            let _ = self.config.save();
                            notice!(&format!(
                                "Switched to \x1b[1m{}\x1b[0m channel. Checking for releases in background...",
                                ch.label()
                            ));
                            if !flashagent_svc::updater::is_dev_mode() {
                                if cx.channel_watch_tx.receiver_count() > 0 {
                                    let _ = cx.channel_watch_tx.send(ch);
                                } else {
                                    let update_tx_clone = cx.update_tx.clone();
                                    tokio::spawn(async move {
                                        if let Ok(Some(target_ver)) = flashagent_svc::updater::check_and_apply_background(ch).await {
                                            let _ = update_tx_clone.send(UpdateNotice::Ready { version: target_ver });
                                        }
                                    });
                                }
                            }
                        } else {
                            notice!("Invalid channel. Choose either: /channel stable or /channel beta");
                        }
                        return Flow::Continue;
                    }

                    if trimmed == "/mcp" || trimmed == "/mcp help" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let mgr = cx.tools_arc.mcp_manager();
                        let paths = mgr.loaded_paths();
                        let statuses = mgr.server_status_list().await;
                        self.effort_menu = None;
                        self.model_menu = None;
                        self.settings_view = None;
                        self.sampling_view = None;
                        self.context_modal = None;
                        self.mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Overview));
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/mcp list" || trimmed == "/mcp ls" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let mgr = cx.tools_arc.mcp_manager();
                        let paths = mgr.loaded_paths();
                        let statuses = mgr.server_status_list().await;
                        self.effort_menu = None;
                        self.model_menu = None;
                        self.settings_view = None;
                        self.sampling_view = None;
                        self.context_modal = None;
                        self.mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Servers));
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/mcp market" || trimmed == "/mcp marketplace" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let mgr = cx.tools_arc.mcp_manager();
                        let paths = mgr.loaded_paths();
                        let statuses = mgr.server_status_list().await;
                        self.effort_menu = None;
                        self.model_menu = None;
                        self.settings_view = None;
                        self.sampling_view = None;
                        self.context_modal = None;
                        self.mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Marketplace));
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if let Some(target) = trimmed.strip_prefix("/mcp test ") {
                        let server_name = target.trim().to_string();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        notice!(&format!("Testing MCP server '{}'...", server_name));
                        let mgr = cx.tools_arc.mcp_manager();
                        match mgr.test_server(&server_name).await {
                            Ok(report) => {
                                let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                                for line in flashagent_tui::mcp_view::render_mcp_test_report(&report, w as usize) {
                                    self.chat.push_line(LineKind::System, line);
                                }
                            }
                            Err(e) => {
                                self.chat.push_system(&format!("\x1b[38;2;245;120;120mMCP server '{server_name}' test failed:\x1b[0m\n{e}"));
                            }
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    } else if trimmed == "/mcp test" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.push_system("Usage: /mcp test <server_name>\nExample: /mcp test sqlite");
                        return Flow::Continue;
                    }

                    if let Some(target) = trimmed.strip_prefix("/mcp add ") {
                        let id = target.trim().to_string();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        if let Some(item) = flashagent_tools::mcp::find_marketplace_item(&id) {
                            let cfg = flashagent_tools::mcp::scaffold_config(item);
                            match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                                Ok(path) => {
                                    let (w, _) = crossterm::terminal::size().unwrap_or((100, 24));
                                    for line in flashagent_tui::mcp_view::render_mcp_add_success(item, &path, w as usize) {
                                        self.chat.push_line(LineKind::System, line);
                                    }
                                    // Start in background
                                    let mgr = cx.tools_arc.mcp_manager();
                                    let server_id = item.id.to_string();
                                    tokio::spawn(async move {
                                        let _ = mgr.reload().await;
                                        let _ = mgr.start_server(&server_id).await;
                                    });
                                }
                                Err(e) => {
                                    notice!(&format!("\x1b[38;2;245;120;120mFailed to save MCP configuration:\x1b[0m {e}"));
                                }
                            }
                        } else {
                            notice!(&format!("Unknown marketplace extension: '{id}'. Type /mcp market to see available items."));
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    } else if trimmed == "/mcp add" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.push_system("Usage: /mcp add <marketplace_id>\nExample: /mcp add sqlite\nType /mcp market to browse extensions.");
                        return Flow::Continue;
                    }

                    if trimmed == "/mcp reload" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let mgr = cx.tools_arc.mcp_manager();
                        match mgr.reload().await {
                            Ok(()) => {
                                let statuses = mgr.server_status_list().await;
                                let active = statuses.iter().filter(|s| s.state == flashagent_tools::mcp::ServerConnectionState::Active).count();
                                let tools: usize = statuses.iter().map(|s| s.tool_count).sum();
                                notice!(&format!(
                                    "\x1b[38;2;135;220;145m✔ MCP reload complete:\x1b[0m {} active server(s), {} discovered tool(s).",
                                    active, tools
                                ));
                            }
                            Err(e) => {
                                notice!(&format!("\x1b[38;2;245;120;120mMCP reload failed:\x1b[0m {e}"));
                            }
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/compact" || trimmed.starts_with("/compact ") {
                        let focus = trimmed.strip_prefix("/compact").map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.push_system("Compacting context...");
                        self.custom_placeholder = None;
                        self.suggested_prompt = None;
                        let tg_speed = self.token_tracker.tg_3s();
                        self.renderer.frame(
                            &self.chat,
                            cx.gate,
                            cx.question_gate,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            &self.context_usage,
                            FrameState {
                                input: &self.input,
                                mode: cx.perm.state().mode(),
                                is_goal_active: self.goal_state.is_some(),
                                goal_progress: None,
                                tip: Some(self.tip_animator.tip_text),
                                tip_animated: None,
                                tip_lines: Some(cx.tip_lines),
                                token_tracker: Some(&self.token_tracker),
                                reasoning_expand: ReasoningExpansion {
                                    all: self.all_expanded,
                                    last: self.last_expanded,
                                },
                                tick_n: self.tick_n,
                                running: self.running,
                                elapsed_secs: self.turn_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                                face_phase: self.turn_started.map(|t| (t.elapsed().as_millis() / 80) as usize).unwrap_or(0),
                                model_tokens: self.token_tracker.total_model_tokens,
                                tokens_per_sec: tg_speed,
                                f_keep: self.token_tracker.last_f_keep,
                                confirm_selection: self.confirm_select.decision(),
                                question_state: Some(&self.question_ui_state),
                                custom_placeholder: self.custom_placeholder.as_deref(),
                                suggested_prompt: self.suggested_prompt.as_deref(),
                                copy_toast: None,
                                prefill_status: None,
                                ttft_display: None,
                                background: self.background.as_ref().map(|b| b.text.as_str()),
                                channel_prompt: None,
                                quit_prompt: None,
                                turn_phase: None,
                                attachments: &[],
        background_style: self.background.as_ref().map_or(NoticeStyle::FULL, BackgroundNotice::style),
                                context_warn_threshold: self.config.context_warn_threshold,
                            },
                        );
                        let before = self.context_usage.total_used();
                        if compact_context(cx.source, &mut self.history, focus.as_deref()).await.is_some() {
                            update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                            let saved = before.saturating_sub(self.context_usage.total_used());
                            // Said in the transcript, like the
                            // automatic one: it is the conversation
                            // that changed.
                            self.chat.replace_last_system(&format!(
                                "Context compacted · {} saved · the conversation so far is now a summary",
                                ContextUsage::format_tokens(saved)
                            ));
                            self.custom_placeholder = None;
                        } else {
                            self.chat.replace_last_system("Nothing to compact yet");
                        }
                        self.suggested_prompt = None;
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/verbose" || trimmed == "/expand" || trimmed == "/think" || trimmed == "/o" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        if !self.last_expanded && !self.all_expanded {
                            self.last_expanded = true;
                            self.all_expanded = false;
                        } else if self.last_expanded && !self.all_expanded {
                            self.last_expanded = false;
                            self.all_expanded = true;
                        } else {
                            self.last_expanded = false;
                            self.all_expanded = false;
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    } else if let Some(arg) = trimmed.strip_prefix("/verbose ")
                        .or_else(|| trimmed.strip_prefix("/expand "))
                        .or_else(|| trimmed.strip_prefix("/think "))
                        .or_else(|| trimmed.strip_prefix("/o "))
                    {
                        let arg_val = arg.trim().to_string();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        match arg_val.as_str() {
                            "all" => {
                                self.all_expanded = true;
                                self.last_expanded = false;
                            }
                            "last" => {
                                self.all_expanded = false;
                                self.last_expanded = true;
                            }
                            "off" | "none" | "collapse" => {
                                self.all_expanded = false;
                                self.last_expanded = false;
                            }
                            _ => {
                                notice!("[Usage: /verbose all | /verbose last | /verbose off]");
                            }
                        };
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/exit" || trimmed == "/quit" || trimmed == "/q" {
                        return Flow::Quit;
                    }

                    if trimmed == "/editor" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        match open_in_external_editor("", &self.config.external_editor) {
                            Ok(edited) => {
                                self.input = edited;
                            }
                            Err(err) => {
                                notice!(&format!("Failed to launch external editor: {err}"));
                            }
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/diff" {
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        match std::process::Command::new("git").args(["diff", "--stat"]).output() {
                            Ok(out) => {
                                let s = String::from_utf8_lossy(&out.stdout);
                                if s.trim().is_empty() {
                                    // `git diff` says nothing about files
                                    // git has never seen, and "clean" next
                                    // to three untracked files is a lie.
                                    let untracked = std::process::Command::new("git")
                                        .args(["ls-files", "--others", "--exclude-standard"])
                                        .output()
                                        .ok()
                                        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
                                        .unwrap_or(0);
                                    match untracked {
                                        0 => notice!("[No changes to tracked files]"),
                                        1 => notice!("[No changes to tracked files · 1 untracked file]"),
                                        n => notice!(&format!("[No changes to tracked files · {n} untracked files]")),
                                    }
                                } else {
                                    self.chat.push_system(&format!("Git diff summary:\n{}", s.trim_end()));
                                }
                            }
                            Err(e) => {
                                notice!(&format!("Failed to run git diff: {e}"));
                            }
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/commit" || trimmed.starts_with("/commit ") {
                        let msg_arg = trimmed.strip_prefix("/commit ").map(|s| s.trim().to_string()).unwrap_or_default();
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        let commit_msg = if !msg_arg.is_empty() {
                            msg_arg.to_string()
                        } else {
                            let ts = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            format!("chore: checkpoint update ({ts})")
                        };
                        match std::process::Command::new("git").args(["commit", "-m", &commit_msg]).output() {
                            Ok(out) if out.status.success() => {
                                let s = String::from_utf8_lossy(&out.stdout);
                                self.chat.push_system(&format!("Git commit succeeded:\n{}", s.trim_end()));
                            }
                            Ok(out) => {
                                let err = String::from_utf8_lossy(&out.stderr);
                                let s = String::from_utf8_lossy(&out.stdout);
                                self.chat.push_system(&format!("Git commit status:\n{}{}", s, err));
                            }
                            Err(e) => {
                                notice!(&format!("Failed to run git commit: {e}"));
                            }
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    if trimmed == "/export" || trimmed.starts_with("/export ") {
                        let format_arg = trimmed.strip_prefix("/export ").map(|s| s.trim().to_lowercase()).unwrap_or_else(|| "md".to_string());
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        // The id already starts with "session_"; the
                        // old line produced session_session_1789.md.
                        let stem = cx.session_id.strip_prefix("session_").unwrap_or(cx.session_id);
                        let filename = match format_arg.as_str() {
                            "html" => format!("session_{stem}.html"),
                            "jsonl" | "json" => format!("session_{stem}.jsonl"),
                            _ => format!("session_{stem}.md"),
                        };
                        let content = match format_arg.as_str() {
                            "html" => {
                                let mut html = String::from("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>FlashAgent Session</title><style>body{font-family:sans-serif;max-width:800px;margin:2rem auto;line-height:1.6;background:#1e1e2e;color:#cdd6f4;}pre{background:#181825;padding:1rem;border-radius:6px;overflow-x:auto;}h3{color:#89b4fa;}</style></head><body>");
                                for m in &self.history {
                                    let role = m.role.as_str();
                                    let escaped = m.content.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
                                    html.push_str(&format!("<h3>Role: {role}</h3><pre>{escaped}</pre>"));
                                }
                                html.push_str("</body></html>");
                                html
                            }
                            "jsonl" | "json" => {
                                let mut buf = String::new();
                                for m in &self.history {
                                    let saved = SavedMessage::from(m);
                                    if let Ok(line) = serde_json::to_string(&saved) {
                                        buf.push_str(&line);
                                        buf.push('\n');
                                    }
                                }
                                buf
                            }
                            _ => {
                                let mut md = format!("# FlashAgent Session Export\n- **Session ID**: `{}`\n- **Model**: `{}`\n\n---\n\n", cx.session_id, self.current_model);
                                for m in &self.history {
                                    md.push_str(&format!("### {}\n\n{}\n\n", m.role.as_str().to_uppercase(), m.content));
                                }
                                md
                            }
                        };
                        match std::fs::write(&filename, content) {
                            Ok(_) => notice!(&format!("Exported conversation to: \x1b[1m{filename}\x1b[0m")),
                            Err(e) => notice!(&format!("Failed to export conversation: {e}")),
                        }
                        self.renderer.request_reprint();
                        return Flow::Continue;
                    }

                    // Check if the command invokes a skill (/skill:<name> or /<name>)
                    let explicit_skill = trimmed.strip_prefix("/skill:").map(|rest| rest.trim().to_string());
                    let skill_name = explicit_skill.clone().or_else(|| {
                        let cand = trimmed.strip_prefix('/').filter(|c| !c.contains(' '))?;
                        find_skill_file(cand).map(|_| cand.to_string())
                    });

                    if let Some(sname) = skill_name {
                        let Some(skill_content) = find_skill_file(&sname).and_then(|p| std::fs::read_to_string(p).ok()) else {
                            self.input.clear();
                            notice!(&format!("[Unknown skill '{sname}'. Type /skills to list available skills.]"));
                            self.renderer.request_reprint();
                            return Flow::Continue;
                        };
                        self.input.clear();
                        self.autocomplete_idx = 0;
                        self.chat.push_user(&format!("/skill:{sname}"));
                        let prompt = format!("Execute skill: {sname}\n\nSkill Instructions:\n{skill_content}");
                        self.history.push(ChatMessage::user(prompt));
                        update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                        cx.cancel.store(false, Ordering::Relaxed);
                        self.suggested_prompt = None;
                        self.custom_placeholder = None;
                        self.running = true;
                        self.turn_phase = TurnPhase::Waiting;
                        self.turn_started = Some(std::time::Instant::now());
                        self.token_tracker.on_turn_start(self.current_model.clone(), self.context_usage.total_used());
                        cx.source.set_model(&self.current_model);
                        cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                        cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                        let turn_opts = build_turn_options(&self.config, &self.current_effort);
                        self.turn_counter += 1;
                        self.turn_outcome = flashagent_core::TurnOutcome::default();
                        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                        self.active_steer_tx = Some(steer_tx);
                        self.active_turn_handle = Some(spawn_turn(
                            cx.cancel.clone(),
                            cx.source.clone(),
                            cx.perm,
                            self.history.clone(),
                            GoalBudgets::steps_only(self.max_steps),
                            turn_opts,
                            cx.tx.clone(),
                            steer_rx,
                            self.turn_counter,
                        ));
                        return Flow::Continue;
                    }

                    let text = std::mem::take(&mut self.input);
                    if self.input_history.last() != Some(&text) {
                        self.input_history.push(text.clone());
                    }
                    self.history_index = None;
                    self.current_draft.clear();
                    // Sending new prompt closes temporary last thinking block (Rule 4)
                    self.last_expanded = false;

                    // What the user types decides what the labels are
                    // written in — in both directions. Detecting only
                    // Russian left an English conversation labelled
                    // "Разбираю запрос" for anyone whose config said
                    // ru, and nothing they typed could change it back.
                    if let Some(lang) = conversation_language(&text) {
                        self.chat.set_language(lang);
                    }
                    // A path typed out is still the user pointing at
                    // a picture — but only a path. Naming a file in a
                    // sentence ("open diagram.png and tell me...") is
                    // a mention, and the model has view_image for
                    // that; silently attaching a megabyte because a
                    // word ended in .png would be a surprise.
                    for token in text.split_whitespace().filter(|t| {
                        t.contains('/') || t.contains('\\')
                    }) {
                        if self.attachments.len() >= 8 {
                            break;
                        }
                        if let Some(att) = Attachment::from_dropped_path(token) {
                            if !self.attachments.iter().any(|a| a.data_url == att.data_url) {
                                self.attachments.push(att);
                            }
                        }
                    }
                    let shown = if self.attachments.is_empty() {
                        text.clone()
                    } else {
                        // The transcript has to show that a picture
                        // went with the message; otherwise the
                        // answer refers to something invisible.
                        let labels: Vec<String> =
                            self.attachments.iter().map(|a| a.label()).collect();
                        format!("{text}  [{}]", labels.join(", "))
                    };
                    self.chat.push_user(&shown);
                    let first = !self.history.iter().any(|m| m.role == flashagent_llm::Role::User);
                    let content = if first && !cx.memory_block.is_empty() {
                        format!("{}\n\n---\n\n{text}", cx.memory_block)
                    } else {
                        text
                    };
                    self.context_before_turn = self.context_usage.total_used();
                    let mut user_msg = ChatMessage::user(content);
                    if !self.attachments.is_empty() {
                        user_msg.images = self.attachments.iter().map(|a| a.data_url.clone()).collect();
                        self.attachments.clear();
                    }
                    self.history.push(user_msg);
                    update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                    cx.cancel.store(false, Ordering::Relaxed);
                    self.suggested_prompt = None;
                    self.custom_placeholder = None;
                    self.running = true;
                    self.turn_phase = TurnPhase::Waiting;
                    self.turn_started = Some(std::time::Instant::now());
                    self.token_tracker.on_turn_start(self.current_model.clone(), self.context_usage.total_used());
                    cx.source.set_model(&self.current_model);
                    cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                    cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                    let turn_opts = build_turn_options(&self.config, &self.current_effort);
                    self.turn_counter += 1;
                        self.turn_outcome = flashagent_core::TurnOutcome::default();
                    let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                    self.active_steer_tx = Some(steer_tx);
                    self.active_turn_handle = Some(spawn_turn(
                        cx.cancel.clone(),
                        cx.source.clone(),
                        cx.perm,
                        self.history.clone(),
                        GoalBudgets::steps_only(self.max_steps),
                        turn_opts,
                        cx.tx.clone(),
                        steer_rx,
                        self.turn_counter,
                    ));
        Flow::Next
    }
}
