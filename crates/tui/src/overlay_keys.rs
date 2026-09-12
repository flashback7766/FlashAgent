use super::*;

impl App {
    /// A key pressed while something sits over the composer — a dialog, a
    /// menu, an approval card, a question — or while the transcript is
    /// scrolled back. `Flow::Next` means none of them claimed it.
    pub(crate) async fn handle_overlay_key(&mut self, cx: &mut LoopCtx<'_>, code: KeyCode, mods: KeyModifiers) -> Flow {
        macro_rules! notice {
            ($text:expr) => {{
                self.custom_placeholder = Some(($text).to_string());
                self.suggested_prompt.take();
                self.renderer.request_reprint();
            }};
        }
        // The channel card owns the keyboard while it is up: it is a
        // yes-or-no about replacing the binary, and typing past it
        // would leave the answer ambiguous.
        if self.quit_confirm {
            self.quit_confirm = false;
            let yes = matches!(
                code,
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('\u{043d}')
                    | KeyCode::Char('\u{041d}') | KeyCode::Enter
            );
            if yes {
                return Flow::Quit;
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        if let Some(sw) = self.channel_switch.take() {
            let yes = matches!(
                code,
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('\u{043d}')
                    | KeyCode::Char('\u{041d}') | KeyCode::Enter
            );
            if yes {
                self.config.update_channel = sw.to;
                let _ = self.config.save();
                if cx.channel_watch_tx.receiver_count() > 0 {
                    let _ = cx.channel_watch_tx.send(sw.to);
                }
                if let Some(ref mut view) = self.settings_view {
                    view.config.update_channel = sw.to;
                }
                self.background = Some(BackgroundNotice::sticky(format!(
                    "Release channel is now {} \u{b7} press Ctrl+U to move to it",
                    sw.to.label()
                )));
            } else {
                // Everything else the user changed stayed applied; only
                // this one is put back.
                if let Some(ref mut view) = self.settings_view {
                    view.config.update_channel = sw.from;
                }
                self.background = Some(BackgroundNotice::fading(
                    format!("Still on the {} channel", sw.from.label()),
                    5,
                ));
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }
        // If settings view is open, it captures all keyboard input
        if let Some(ref mut settings) = self.settings_view {
            // What the view was opened with (live session values).
            let shown_mode = self.goal_state.as_ref().map_or(cx.perm.state().mode(), |g| g.mode);
            let shown_effort = self.goal_state.as_ref().map_or(self.current_effort.clone(), |g| g.effort.clone());
            let action = settings.handle_key(code, mods);
            match action {
                SettingsAction::Close => {
                    let old_model = self.current_model.clone();
                    let old_effort = shown_effort.clone();
                    let old_mode = shown_mode;
                    let chosen_mode = settings.config.permission_mode;
                    let chosen_effort = settings.config.thinking_effort.clone();
                    let mut changes = Vec::new();
                    if settings.config.permission_mode != old_mode {
                        changes.push(format!("mode ({})", settings.config.permission_mode.label()));
                    }
                    if settings.config.model != old_model {
                        changes.push(format!("model ({})", settings.config.model));
                    }
                    if settings.config.thinking_effort != old_effort {
                        changes.push(format!("thinking ({})", settings.config.thinking_effort));
                    }

                    if changes.len() == 1 && settings.config.permission_mode != old_mode {
                        self.custom_placeholder = Some(format!("Permission mode set to: {}", settings.config.permission_mode.label()));
                    } else if !changes.is_empty() {
                        self.custom_placeholder = Some(format!("Settings updated: {}", changes.join(", ")));
                    }
                    self.suggested_prompt = None;

                    // Every other setting is applied now; the release
                    // channel waits for an answer, so declining costs
                    // the user nothing else they just changed.
                    let wanted_channel = settings.config.update_channel;
                    let mut applied = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                    if wanted_channel != self.config.update_channel {
                        applied.update_channel = self.config.update_channel;
                        self.custom_placeholder = None;
                        self.channel_switch = Some(ChannelSwitch {
                            from: self.config.update_channel,
                            to: wanted_channel,
                            target: ChannelTarget::Checking,
                        });
                        let tx_ch = cx.channel_probe_tx.clone();
                        tokio::spawn(async move {
                            let target = match flashagent_svc::updater::newest_on_channel(
                                wanted_channel,
                                flashagent_svc::updater::DEFAULT_RELEASES_API,
                            )
                            .await
                            {
                                Ok(Some(version)) => ChannelTarget::Version(version),
                                Ok(None) => ChannelTarget::Empty,
                                Err(_) => ChannelTarget::Unknown,
                            };
                            let _ = tx_ch.send(target);
                        });
                    }
                    self.config = applied;
                    let _ = self.config.save();
                    cx.tools_arc.set_toolset_profile(self.config.toolset_profile);
                    cx.tools_arc.set_web_enabled(self.config.free_search);
                    cx.source.0.set_max_retries(self.config.network_retries);
                    if self.config.model != self.current_model {
                        self.current_model = self.config.model.clone();
                        cx.source.set_model(&self.current_model);
                        cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                        cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                    }
                    // During /goal the live mode/effort are the goal's;
                    // edits apply to what the goal restores afterwards.
                    match self.goal_state.as_mut() {
                        Some(g) => {
                            g.mode = chosen_mode;
                            g.effort = chosen_effort;
                        }
                        None => {
                            cx.perm.state().set_mode(chosen_mode);
                            self.current_effort = chosen_effort;
                        }
                    }
                    self.settings_view = None;
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
                    self.renderer.request_reprint();
                }
                SettingsAction::DiscoverModels => {
                    self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                    let _ = self.config.save();
                    // Probe the URL the user just typed, not the one
                    // this session is connected to.
                    let key = settings.config.api_key.clone().or_else(|| std::env::var("FLASHAGENT_API_KEY").ok());
                    let probe = flashagent_llm::OpenAiCompat::new(&settings.config.backend_url, "", key);
                    settings.available_models = match probe.discover_server().await {
                        Some(disc) => disc.models.into_iter().map(|m| m.id).collect(),
                        None => Vec::new(),
                    };
                    if settings.config.backend_url.trim_end_matches('/') != cx.source.0.base_url() {
                        self.custom_placeholder = Some("Backend URL saved; restart FlashAgent to connect to it".to_string());
                    }
                    self.renderer.request_reprint();
                }
                SettingsAction::RunToolTest => {
                    settings.tool_test_status = Some(format!("Probing {}...", self.current_model));
                    let source_bg = cx.source.clone();
                    let tx_bg = cx.tx.clone();
                    tokio::spawn(async move {
                        let verdict = run_tool_call_probe(&source_bg).await;
                        let _ = tx_bg.send(UiEvent::ToolTestResult(verdict));
                    });
                    self.renderer.request_reprint();
                }
                SettingsAction::OpenModelMenu => {
                    self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                    let _ = self.config.save();
                    self.settings_view = None;
                    if let Some(mut menu) = build_model_menu(cx.source) {
                        menu.select_by_value(&self.current_model);
                        self.model_menu = Some(menu);
                    } else {
                        notice!("[No models discovered from server]");
                    }
                    self.renderer.request_reprint();
                }
                SettingsAction::OpenEffortMenu => {
                    self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                    let _ = self.config.save();
                    self.settings_view = None;
                    let mut menu = build_effort_menu(cx.source, &self.effort_memory, &self.current_model);
                    menu.select_by_value(&self.current_effort);
                    self.effort_menu = Some(menu);
                    self.renderer.request_reprint();
                }
                SettingsAction::OpenWizard => {
                    self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                    let _ = self.config.save();
                    self.settings_view = None;
                    let completed = flashagent_tui::run_wizard_channel(&mut self.config, cx.rx).await.unwrap_or(false);
                    if completed {
                        self.current_model = self.config.model.clone();
                        cx.source.set_model(&self.current_model);
                        cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                        cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                        self.current_effort = self.config.thinking_effort.clone();
                        cx.perm.state().set_mode(self.config.permission_mode);
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
                    }
                    self.renderer.request_reprint();
                }
                SettingsAction::OpenSamplingMenu => {
                    self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                    let _ = self.config.save();
                    self.settings_view = None;
                    self.sampling_view = Some(SamplingView::new(&self.config));
                    self.renderer.request_reprint();
                }
                SettingsAction::CheckUpdatesNow => {
                    if flashagent_svc::updater::is_dev_mode() {
                        settings.update_check_status = Some("Dev mode: updates disabled (source build)".into());
                    } else {
                        settings.update_check_status = Some("Checking GitHub releases...".into());
                        let ch = settings.config.update_channel;
                        let update_tx_clone = cx.update_tx.clone();
                        tokio::spawn(async move {
                            match flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                                Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                    let _ = update_tx_clone.send(UpdateNotice::Available { version: target, asset_name, download_url, checksums_url });
                                }
                                Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => {
                                    let _ = update_tx_clone.send(UpdateNotice::UpToDate { version: current });
                                }
                                Err(e) => {
                                    let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                                }
                            }
                        });
                    }
                    self.renderer.request_reprint();
                }
                SettingsAction::OpenMcpMenu => {
                    self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                    let _ = self.config.save();
                    self.settings_view = None;
                    let mgr = cx.tools_arc.mcp_manager();
                    let paths = mgr.loaded_paths();
                    let statuses = mgr.server_status_list().await;
                    self.mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Overview));
                    self.renderer.request_reprint();
                }
                SettingsAction::None => {
                    self.renderer.request_reprint();
                }
            }
            return Flow::Continue;
        }

        // If sampling parameters view is open, it captures all keyboard input
        if let Some(ref mut sm) = self.sampling_view {
            let action = sm.handle_key(code, mods);
            match action {
                SamplingAction::Close => {
                    self.sampling_view = None;
                    self.renderer.request_reprint();
                }
                SamplingAction::SaveAndClose => {
                    sm.apply_to_config(&mut self.config);
                    let _ = self.config.save();
                    self.sampling_view = None;
                    self.custom_placeholder = Some("Sampling parameters updated".to_string());
                    self.suggested_prompt = None;
                    self.renderer.request_reprint();
                }
                SamplingAction::None => {
                    self.renderer.request_reprint();
                }
            }
            return Flow::Continue;
        }

        // The memory screen owns every key while it is up: 'd' and
        // 'e' are commands on the list and letters inside a note.
        if let Some(ref mut modal) = self.memory_modal {
            use flashagent_tui::memory_view::MemoryAction;
            match modal.handle_key(code, mods) {
                MemoryAction::None => {}
                MemoryAction::Close => {
                    self.memory_modal = None;
                }
                MemoryAction::Forget { name, scope } => {
                    let cwd = std::env::current_dir().unwrap_or_default();
                    let removed = flashagent_core::MemoryStore::for_scope(scope, &cwd)
                        .map(|store| store.remove(&name).unwrap_or(false))
                        .unwrap_or(false);
                    notice!(&if removed {
                        format!("[Forgot \"{name}\"]")
                    } else {
                        format!("[Could not forget \"{name}\"]")
                    });
                    self.memory_modal = Some(flashagent_tui::memory_view::MemoryModal::new(&cwd));
                }
                MemoryAction::Tell { message } => {
                    // Sent through the ordinary path, so it is an
                    // ordinary turn: the model decides what to change
                    // and says so in the chat.
                    self.memory_modal = None;
                    self.input = message;
                    let _ = cx.tx.send(UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE));
                }
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        // If context modal is open, F1, Enter, Esc or 'q' closes it
        if self.context_modal.is_some() {
            if matches!(code, KeyCode::F(1) | KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q')) {
                self.context_modal = None;
                self.renderer.request_reprint();
            }
            return Flow::Continue;
        }

        // If MCP modal is open, it captures navigation and actions
        if let Some(ref mut modal) = self.mcp_modal {
            match modal.handle_key(code, mods) {
                McpModalAction::Close => {
                    self.mcp_modal = None;
                    self.renderer.request_reprint();
                }
                McpModalAction::Reload => {
                    let mgr = cx.tools_arc.mcp_manager();
                    let _ = mgr.reload().await;
                    modal.servers = mgr.server_status_list().await;
                    modal.status_message = Some("Reloaded MCP configurations".to_string());
                    self.renderer.request_reprint();
                }
                McpModalAction::TestServer(name) => {
                    modal.status_message = Some(format!("Testing {name}..."));
                    self.renderer.request_reprint();
                    let mgr = cx.tools_arc.mcp_manager();
                    match mgr.test_server(&name).await {
                        Ok(report) => {
                            let ver = report.server_version.as_deref().unwrap_or("1.0.0");
                            let lat = format!("{:.1}ms", report.latency.as_secs_f64() * 1000.0);
                            modal.status_message = Some(format!("✔ {name} connected (v{ver}, {lat}, {} tools)", report.tools.len()));
                        }
                        Err(e) => {
                            modal.status_message = Some(format!("✕ {name} failed: {e}"));
                        }
                    }
                    modal.servers = mgr.server_status_list().await;
                    self.renderer.request_reprint();
                }
                McpModalAction::InstallMarketplace(id) => {
                    if let Some(item) = flashagent_tools::mcp::find_marketplace_item(&id) {
                        let cfg = flashagent_tools::mcp::scaffold_config(item);
                        match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                            Ok(path) => {
                                modal.status_message = Some(format!("✔ Added {} to {}", item.name, path.display()));
                                let mgr = cx.tools_arc.mcp_manager();
                                let server_id = item.id.to_string();
                                let mgr_clone = mgr.clone();
                                tokio::spawn(async move {
                                    let _ = mgr_clone.reload().await;
                                    let _ = mgr_clone.start_server(&server_id).await;
                                });
                            }
                            Err(e) => {
                                modal.status_message = Some(format!("✕ Failed to install {id}: {e}"));
                            }
                        }
                    }
                    self.renderer.request_reprint();
                }
                McpModalAction::None => {
                    self.renderer.request_reprint();
                }
            }
            return Flow::Continue;
        }

        // If model selection menu is open, it captures navigation
        if let Some(ref mut menu) = self.model_menu {
            match code {
                KeyCode::Up => menu.up(),
                KeyCode::Down => menu.down(),
                KeyCode::PageUp => menu.page_up(),
                KeyCode::PageDown => menu.page_down(),
                KeyCode::Backspace => menu.pop_filter_char(),
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                    menu.push_filter_char(c);
                }
                KeyCode::Enter => {
                    if let Some(val) = menu.selected_value() {
                        self.current_model = val.clone();
                        self.config.model = self.current_model.clone();
                        let _ = self.config.save();
                        cx.source.set_model(&self.current_model);
                        cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                        cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                        if let Some(disc) = cx.source.discovery() {
                            if let Some(m) = disc.models.iter().find(|m| m.id == self.current_model) {
                                self.current_context = m.context_display();
                                // The effort the user picked survives
                                // the switch; a model that cannot
                                // reason just receives no thinking
                                // fields.
                                if self.current_effort.is_empty() {
                                    self.current_effort = "auto".to_string();
                                }
                            }
                        }
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
                        self.custom_placeholder = Some(format!("Switched active model to: {}", self.current_model));
                        self.suggested_prompt = None;
                        self.renderer.request_reprint();
                    }
                    self.model_menu = None;
                }
                KeyCode::Esc => {
                    self.model_menu = None;
                }
                _ => {}
            }
            return Flow::Continue;
        }

        // If effort selection menu is open, it captures navigation
        if let Some(ref mut menu) = self.effort_menu {
            match code {
                KeyCode::Up => menu.up(),
                KeyCode::Down => menu.down(),
                KeyCode::Enter => {
                    if let Some(val) = menu.selected_value() {
                        self.current_effort = val.clone();
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
                        self.custom_placeholder = Some(format!("Thinking effort set to: {}", self.current_effort));
                        self.suggested_prompt = None;
                        self.renderer.request_reprint();
                    }
                    self.effort_menu = None;
                }
                KeyCode::Esc => {
                    self.effort_menu = None;
                }
                _ => {}
            }
            return Flow::Continue;
        }

        if let Some(req) = cx.question_gate.pending() {
            match code {
                // Ctrl+C interrupts the turn, as everywhere else while it runs.
                KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                    if mods.contains(KeyModifiers::CONTROL) =>
                {
                    self.question_ui_state = QuestionUiState::default();
                    cx.question_gate.cancel();
                    if self.running && self.cancel_requested.is_none() {
                        cx.cancel.store(true, Ordering::Relaxed);
                        self.cancel_requested = Some(std::time::Instant::now());
                        self.turn_phase = TurnPhase::Stopping;
                        self.active_steer_tx = None;
                        self.custom_placeholder = Some("Interrupting...".to_string());
                    }
                }
                KeyCode::Esc => {
                    if req.options.is_some() && self.question_ui_state.is_writing {
                        self.question_ui_state.is_writing = false;
                    } else {
                        self.question_ui_state = QuestionUiState::default();
                        cx.question_gate.cancel();
                    }
                }
                KeyCode::Enter => {
                    if let Some(ref opts) = req.options {
                        let total_choices = opts.len();
                        if self.question_ui_state.is_writing {
                            let answer = std::mem::take(&mut self.question_ui_state.write_in_text);
                            let trimmed = answer.trim().to_string();
                            if !trimmed.is_empty() {
                                if req.multi_select && !self.question_ui_state.selected_indices.is_empty() {
                                    let mut chosen: Vec<String> = self.question_ui_state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect();
                                    chosen.push(trimmed);
                                    self.question_ui_state = QuestionUiState::default();
                                    cx.question_gate.respond(chosen.join(", "), true);
                                } else {
                                    self.question_ui_state = QuestionUiState::default();
                                    cx.question_gate.respond(trimmed, true);
                                }
                            }
                        } else if self.question_ui_state.selected_index == total_choices {
                            self.question_ui_state.is_writing = true;
                            self.question_ui_state.write_in_text.clear();
                        } else if req.multi_select {
                            let mut chosen: Vec<String> = self.question_ui_state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect();
                            if chosen.is_empty() && self.question_ui_state.selected_index < total_choices {
                                chosen.push(opts[self.question_ui_state.selected_index].clone());
                            }
                            self.question_ui_state = QuestionUiState::default();
                            cx.question_gate.respond(chosen.join(", "), false);
                        } else if self.question_ui_state.selected_index < total_choices {
                            let chosen = opts[self.question_ui_state.selected_index].clone();
                            self.question_ui_state = QuestionUiState::default();
                            cx.question_gate.respond(chosen, false);
                        }
                    } else {
                        let answer = std::mem::take(&mut self.question_ui_state.write_in_text);
                        let trimmed = answer.trim().to_string();
                        self.question_ui_state = QuestionUiState::default();
                        cx.question_gate.respond(trimmed, true);
                    }
                }
                KeyCode::Up => {
                    if !self.question_ui_state.is_writing {
                        if let Some(ref opts) = req.options {
                            let total = opts.len() + 1;
                            if self.question_ui_state.selected_index == 0 {
                                self.question_ui_state.selected_index = total.saturating_sub(1);
                            } else {
                                self.question_ui_state.selected_index -= 1;
                            }
                        }
                    }
                }
                KeyCode::Down => {
                    if !self.question_ui_state.is_writing {
                        if let Some(ref opts) = req.options {
                            let total = opts.len() + 1;
                            self.question_ui_state.selected_index = (self.question_ui_state.selected_index + 1) % total;
                        }
                    }
                }
                KeyCode::Backspace => {
                    if self.question_ui_state.is_writing || req.options.is_none() {
                        self.question_ui_state.write_in_text.pop();
                    }
                }
                KeyCode::Char(' ') if req.multi_select && !self.question_ui_state.is_writing => {
                    if let Some(ref opts) = req.options {
                        let total_choices = opts.len();
                        if self.question_ui_state.selected_index < total_choices {
                            let idx = self.question_ui_state.selected_index;
                            if self.question_ui_state.selected_indices.contains(&idx) {
                                self.question_ui_state.selected_indices.remove(&idx);
                            } else {
                                self.question_ui_state.selected_indices.insert(idx);
                            }
                        } else {
                            self.question_ui_state.is_writing = true;
                            self.question_ui_state.write_in_text.clear();
                        }
                    }
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                    if self.question_ui_state.is_writing || req.options.is_none() {
                        self.question_ui_state.write_in_text.push(c);
                    } else if let Some(ref opts) = req.options {
                        let total = opts.len() + 1;
                        if let Some(d) = c.to_digit(10) {
                            let idx = (d as usize).saturating_sub(1);
                            if idx < opts.len() {
                                if req.multi_select {
                                    if self.question_ui_state.selected_indices.contains(&idx) {
                                        self.question_ui_state.selected_indices.remove(&idx);
                                    } else {
                                        self.question_ui_state.selected_indices.insert(idx);
                                    }
                                }
                                self.question_ui_state.selected_index = idx;
                            } else if idx == total - 1 {
                                self.question_ui_state.selected_index = idx;
                                self.question_ui_state.is_writing = true;
                                self.question_ui_state.write_in_text.clear();
                            }
                        }
                    }
                }
                _ => {}
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        if cx.gate.pending().is_some() {
            match code {
                // Ctrl+C refuses the call and interrupts the turn.
                KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                    if mods.contains(KeyModifiers::CONTROL) =>
                {
                    cx.gate.respond(Decision::Deny);
                    self.confirm_select = ConfirmSelect::new();
                    if self.running && self.cancel_requested.is_none() {
                        cx.cancel.store(true, Ordering::Relaxed);
                        self.cancel_requested = Some(std::time::Instant::now());
                        self.turn_phase = TurnPhase::Stopping;
                        self.active_steer_tx = None;
                        self.custom_placeholder = Some("Interrupting...".to_string());
                    }
                }
                KeyCode::Esc
                | KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('\u{0432}') | KeyCode::Char('\u{0412}') => {
                    cx.gate.respond(Decision::Deny);
                    self.confirm_select = ConfirmSelect::new();
                }
                KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('\u{0444}') | KeyCode::Char('\u{0424}') => {
                    if let Some(req) = cx.gate.pending() {
                        // Narrow rules for shell (`npm test` never
                        // covers `npm publish`); tool-wide otherwise.
                        let rules = cx.perm.state().allow_always(&req);
                        if rules.is_empty() {
                            notice!("[Allowed once: this command cannot be saved as a narrow rule]");
                        } else {
                            notice!(&format!("[Always allowed this session: {}]", rules.join(", ")));
                        }
                    }
                    cx.gate.respond(Decision::Allow);
                    self.confirm_select = ConfirmSelect::new();
                }
                KeyCode::Enter => {
                    cx.gate.respond(self.confirm_select.decision());
                    self.confirm_select = ConfirmSelect::new();
                }
                KeyCode::Left => {
                    self.confirm_select.left();
                }
                KeyCode::Right => {
                    self.confirm_select.right();
                }
                KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
                    self.confirm_select.toggle();
                }
                _ => {}
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        // --- Chat History Scrolling & Navigation ---
        if matches!(code, KeyCode::PageUp) {
            let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
            let total_chat_lines = self.chat.render_split(width as usize, ReasoningExpansion { all: self.all_expanded, last: self.last_expanded }).0.len() + 15;
            let max_scroll = total_chat_lines.saturating_sub(height as usize);
            self.renderer.scroll_up((height as usize / 2).max(5), max_scroll);
            return Flow::Continue;
        }
        if matches!(code, KeyCode::PageDown) {
            let (_, height) = crossterm::terminal::size().unwrap_or((100, 24));
            self.renderer.scroll_down((height as usize / 2).max(5));
            return Flow::Continue;
        }
        if matches!(code, KeyCode::Home) && self.renderer.scroll_offset > 0 {
            let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
            let total_chat_lines = self.chat.render_split(width as usize, ReasoningExpansion { all: self.all_expanded, last: self.last_expanded }).0.len() + 15;
            let max_scroll = total_chat_lines.saturating_sub(height as usize);
            self.renderer.scroll_up(max_scroll, max_scroll);
            return Flow::Continue;
        }
        if matches!(code, KeyCode::End) && self.renderer.scroll_offset > 0 {
            self.renderer.scroll_to_bottom();
            return Flow::Continue;
        }
        if matches!(code, KeyCode::Esc) && self.renderer.scroll_offset > 0 {
            self.renderer.scroll_to_bottom();
            return Flow::Continue;
        }
        // Shift+Up, Ctrl+Up, Alt+Up -> scroll chat up.
        // Once scrolled back, a plain Up keeps scrolling the transcript.
        if (matches!(code, KeyCode::Up) && (mods.contains(KeyModifiers::SHIFT) || mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)))
            || (self.renderer.scroll_offset > 0 && matches!(code, KeyCode::Up))
        {
            let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
            let total_chat_lines = self.chat.render_split(width as usize, ReasoningExpansion { all: self.all_expanded, last: self.last_expanded }).0.len() + 15;
            let max_scroll = total_chat_lines.saturating_sub(height as usize);
            self.renderer.scroll_up(2, max_scroll);
            return Flow::Continue;
        }
        // Shift+Down, Ctrl+Down, Alt+Down -> scroll chat down.
        // Once scrolled back, a plain Down scrolls toward the end.
        if (matches!(code, KeyCode::Down) && (mods.contains(KeyModifiers::SHIFT) || mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)))
            || (self.renderer.scroll_offset > 0 && matches!(code, KeyCode::Down))
        {
            self.renderer.scroll_down(2);
            return Flow::Continue;
        }

        if self.renderer.scroll_offset > 0 && matches!(code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter) {
            self.renderer.scroll_to_bottom();
        }
        Flow::Next
    }
}
