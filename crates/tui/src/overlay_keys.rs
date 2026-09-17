use super::*;

/// Enter or y (Latin or Cyrillic) on a yes-or-no card.
fn is_yes(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('\u{043d}') | KeyCode::Char('\u{041d}') | KeyCode::Enter
    )
}

/// Ctrl+C, Latin or Cyrillic layout.
pub(crate) fn is_ctrl_c(code: KeyCode, mods: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}'))
        && mods.contains(KeyModifiers::CONTROL)
}

impl App {
    /// A key pressed while something sits over the composer — an approval
    /// card, a yes-or-no card, a question, a menu or screen — or while the
    /// transcript is scrolled back. `Flow::Next` means none of them claimed it.
    ///
    /// They are asked in the order they are drawn on top of each other, so the
    /// key always goes to the card the user is looking at: an approval that
    /// comes up over open settings takes Enter, not the settings under it.
    pub(crate) async fn handle_overlay_key(&mut self, cx: &mut LoopCtx<'_>, code: KeyCode, mods: KeyModifiers) -> Flow {
        if cx.gate.pending().is_some() {
            self.approval_key(cx, code, mods);
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        // The uninstall card: yes closes the app, and the uninstaller runs in
        // the restored terminal, where it asks what data to delete.
        if self.uninstall_confirm {
            self.uninstall_confirm = false;
            if is_yes(code) {
                UNINSTALL_AFTER_EXIT.store(true, Ordering::SeqCst);
                return Flow::Quit;
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        // The channel card owns the keyboard while it is up: it is a
        // yes-or-no about replacing the binary, and typing past it would
        // leave the answer ambiguous.
        if let Some(sw) = self.channel_switch.take() {
            self.channel_switch_key(cx, sw, code);
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        if let Some(req) = cx.question_gate.pending() {
            self.question_key(cx, &req, code, mods);
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        // The open panel is taken out while its key is handled and put back
        // unless the key closed it or opened another.
        if let Some(overlay) = self.overlay.take() {
            match overlay {
                Overlay::Settings(view) => self.settings_key(cx, view, code, mods).await,
                Overlay::Sampling(view) => self.sampling_key(view, code, mods),
                Overlay::Memory(modal) => self.memory_key(cx, modal, code, mods),
                Overlay::Context(modal) => {
                    // F1, Enter, Esc or q close it; anything else leaves it up.
                    if !matches!(code, KeyCode::F(1) | KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q')) {
                        self.overlay = Some(Overlay::Context(modal));
                    }
                }
                Overlay::Mcp(modal) => self.mcp_key(cx, modal, code, mods).await,
                Overlay::Sessions(menu) => self.session_menu_key(menu, code, mods),
                Overlay::Model(menu) => self.model_menu_key(cx, menu, code, mods),
                Overlay::Rewind(card) => self.rewind_key(cx, card, code),
                Overlay::Effort(menu) => self.effort_menu_key(cx, menu, code),
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        self.scroll_key(code, mods)
    }

    fn approval_key(&mut self, cx: &LoopCtx<'_>, code: KeyCode, mods: KeyModifiers) {
        match code {
            // Ctrl+C refuses the call and interrupts the turn.
            _ if is_ctrl_c(code, mods) => {
                cx.gate.respond(Decision::Deny);
                self.confirm_select = ConfirmSelect::new();
                self.interrupt(cx);
            }
            KeyCode::Esc | KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('\u{0432}') | KeyCode::Char('\u{0412}') => {
                cx.gate.respond(Decision::Deny);
                self.confirm_select = ConfirmSelect::new();
            }
            KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('\u{0444}') | KeyCode::Char('\u{0424}') => {
                if let Some(req) = cx.gate.pending() {
                    // Narrow rules for shell (`npm test` never covers
                    // `npm publish`); tool-wide otherwise.
                    let rules = cx.perm.state().allow_always(&req);
                    if rules.is_empty() {
                        self.notice("[Allowed once: this command cannot be saved as a narrow rule]");
                    } else {
                        self.notice(format!("[Always allowed this session: {}]", rules.join(", ")));
                    }
                }
                cx.gate.respond(Decision::Allow);
                self.confirm_select = ConfirmSelect::new();
            }
            KeyCode::Enter => {
                cx.gate.respond(self.confirm_select.decision());
                self.confirm_select = ConfirmSelect::new();
            }
            KeyCode::Left => self.confirm_select.left(),
            KeyCode::Right => self.confirm_select.right(),
            KeyCode::Tab | KeyCode::Up | KeyCode::Down => self.confirm_select.toggle(),
            _ => {}
        }
    }

    fn channel_switch_key(&mut self, cx: &LoopCtx<'_>, sw: ChannelSwitch, code: KeyCode) {
        if is_yes(code) {
            self.config.update_channel = sw.to;
            let _ = self.config.save();
            if cx.channel_watch_tx.receiver_count() > 0 {
                let _ = cx.channel_watch_tx.send(sw.to);
            }
            if let Some(view) = self.settings_view_mut() {
                view.config.update_channel = sw.to;
            }
            self.background = Some(BackgroundNotice::sticky(format!(
                "Release channel is now {} \u{b7} press Ctrl+U to move to it",
                sw.to.label()
            )));
        } else {
            // Everything else the user changed stayed applied; only this one
            // is put back.
            if let Some(view) = self.settings_view_mut() {
                view.config.update_channel = sw.from;
            }
            self.background = Some(BackgroundNotice::fading(format!("Still on the {} channel", sw.from.label()), 5));
        }
    }

    fn question_key(&mut self, cx: &LoopCtx<'_>, req: &flashagent_tui::QuestionRequest, code: KeyCode, mods: KeyModifiers) {
        if is_ctrl_c(code, mods) {
            // Ctrl+C interrupts the turn, as everywhere else while it runs.
            self.question_ui_state = QuestionUiState::default();
            cx.question_gate.cancel();
            self.interrupt(cx);
            return;
        }
        let state = &mut self.question_ui_state;
        match code {
            KeyCode::Esc => {
                if req.options.is_some() && state.is_writing {
                    state.is_writing = false;
                } else {
                    *state = QuestionUiState::default();
                    cx.question_gate.cancel();
                }
            }
            KeyCode::Enter => match &req.options {
                Some(opts) => {
                    let total_choices = opts.len();
                    if state.is_writing {
                        let answer = std::mem::take(&mut state.write_in_text);
                        let trimmed = answer.trim().to_string();
                        if !trimmed.is_empty() {
                            let mut chosen: Vec<String> = if req.multi_select {
                                state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect()
                            } else {
                                Vec::new()
                            };
                            chosen.push(trimmed);
                            *state = QuestionUiState::default();
                            cx.question_gate.respond(chosen.join(", "), true);
                        }
                    } else if state.selected_index == total_choices {
                        state.is_writing = true;
                        state.write_in_text.clear();
                    } else if req.multi_select {
                        let mut chosen: Vec<String> =
                            state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect();
                        if chosen.is_empty() && state.selected_index < total_choices {
                            chosen.push(opts[state.selected_index].clone());
                        }
                        *state = QuestionUiState::default();
                        cx.question_gate.respond(chosen.join(", "), false);
                    } else if state.selected_index < total_choices {
                        let chosen = opts[state.selected_index].clone();
                        *state = QuestionUiState::default();
                        cx.question_gate.respond(chosen, false);
                    }
                }
                None => {
                    let answer = std::mem::take(&mut state.write_in_text);
                    *state = QuestionUiState::default();
                    cx.question_gate.respond(answer.trim().to_string(), true);
                }
            },
            KeyCode::Up => {
                if let (false, Some(opts)) = (state.is_writing, &req.options) {
                    let total = opts.len() + 1;
                    state.selected_index = state.selected_index.checked_sub(1).unwrap_or(total - 1);
                }
            }
            KeyCode::Down => {
                if let (false, Some(opts)) = (state.is_writing, &req.options) {
                    state.selected_index = (state.selected_index + 1) % (opts.len() + 1);
                }
            }
            KeyCode::Backspace => {
                if state.is_writing || req.options.is_none() {
                    state.write_in_text.pop();
                }
            }
            KeyCode::Char(' ') if req.multi_select && !state.is_writing => {
                if let Some(opts) = &req.options {
                    let idx = state.selected_index;
                    if idx < opts.len() {
                        if !state.selected_indices.remove(&idx) {
                            state.selected_indices.insert(idx);
                        }
                    } else {
                        state.is_writing = true;
                        state.write_in_text.clear();
                    }
                }
            }
            KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                if state.is_writing || req.options.is_none() {
                    state.write_in_text.push(c);
                } else if let (Some(opts), Some(d)) = (&req.options, c.to_digit(10)) {
                    let idx = (d as usize).saturating_sub(1);
                    if idx < opts.len() {
                        if req.multi_select && !state.selected_indices.remove(&idx) {
                            state.selected_indices.insert(idx);
                        }
                        state.selected_index = idx;
                    } else if idx == opts.len() {
                        state.selected_index = idx;
                        state.is_writing = true;
                        state.write_in_text.clear();
                    }
                }
            }
            _ => {}
        }
    }

    async fn settings_key(&mut self, cx: &mut LoopCtx<'_>, mut settings: Box<SettingsView>, code: KeyCode, mods: KeyModifiers) {
        // What the view was opened with (live session values).
        let shown_mode = self.goal_state.as_ref().map_or(cx.perm.state().mode(), |g| g.mode);
        let shown_effort = self.goal_state.as_ref().map_or(self.current_effort.clone(), |g| g.effort.clone());
        match settings.handle_key(code, mods) {
            SettingsAction::Close => self.close_settings(cx, settings, shown_mode, &shown_effort),
            SettingsAction::DiscoverModels => {
                self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                let _ = self.config.save();
                // Probe the URL the user just typed, not the one this session
                // is connected to.
                let key = settings.config.api_key.clone().or_else(|| std::env::var("FLASHAGENT_API_KEY").ok());
                let probe = flashagent_llm::OpenAiCompat::new(&settings.config.backend_url, "", key);
                settings.available_models = match probe.discover_server().await {
                    Some(disc) => disc.models.into_iter().map(|m| m.id).collect(),
                    None => Vec::new(),
                };
                if settings.config.backend_url.trim_end_matches('/') != cx.source.0.base_url() {
                    self.custom_placeholder = Some("Backend URL saved; restart FlashAgent to connect to it".to_string());
                }
                self.overlay = Some(Overlay::Settings(settings));
            }
            SettingsAction::RunToolTest => {
                settings.tool_test_status = Some(format!("Probing {}...", self.current_model));
                let source_bg = cx.source.clone();
                let tx_bg = cx.tx.clone();
                tokio::spawn(async move {
                    let verdict = run_tool_call_probe(&source_bg).await;
                    let _ = tx_bg.send(UiEvent::ToolTestResult(verdict));
                });
                self.overlay = Some(Overlay::Settings(settings));
            }
            SettingsAction::OpenModelMenu => {
                self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                let _ = self.config.save();
                self.open_model_menu(cx.source);
            }
            SettingsAction::OpenEffortMenu => {
                self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                let _ = self.config.save();
                self.open_effort_menu(cx.source);
            }
            SettingsAction::OpenWizard => {
                self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                let _ = self.config.save();
                let mut deferred = Vec::new();
                let completed =
                    flashagent_tui::run_wizard_channel(&mut self.config, cx.rx, &mut deferred).await.unwrap_or(false);
                for ev in deferred {
                    let _ = cx.tx.send(ev);
                }
                if completed {
                    self.current_model = self.config.model.clone();
                    cx.source.set_model(&self.current_model);
                    cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                    cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                    self.current_effort = self.config.thinking_effort.clone();
                    cx.perm.state().set_mode(self.config.permission_mode);
                    self.refresh_welcome(cx.source, cx.perm.state().mode(), cx.mascot_mood);
                }
            }
            SettingsAction::OpenSamplingMenu => {
                self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                let _ = self.config.save();
                self.overlay = Some(Overlay::Sampling(SamplingView::new(&self.config)));
            }
            SettingsAction::CheckUpdatesNow => {
                if flashagent_svc::updater::is_dev_mode() {
                    settings.update_check_status = Some("Dev mode: updates disabled (source build)".into());
                } else {
                    settings.update_check_status = Some("Checking GitHub releases...".into());
                    let ch = settings.config.update_channel;
                    let update_tx = cx.update_tx.clone();
                    tokio::spawn(async move {
                        use flashagent_svc::updater::{check_for_updates, UpdateStatus, DEFAULT_RELEASES_API};
                        let notice = match check_for_updates(ch, DEFAULT_RELEASES_API).await {
                            Ok(UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                UpdateNotice::Available { version: target, asset_name, download_url, checksums_url }
                            }
                            Ok(UpdateStatus::UpToDate { current, .. }) => UpdateNotice::UpToDate { version: current },
                            Err(e) => UpdateNotice::Failed { error: e.to_string() },
                        };
                        let _ = update_tx.send(notice);
                    });
                }
                self.overlay = Some(Overlay::Settings(settings));
            }
            SettingsAction::OpenMcpMenu => {
                self.config = persisted_from_view(&settings.config, &self.config, shown_mode, &shown_effort);
                let _ = self.config.save();
                self.open_mcp(cx, McpViewTab::Overview).await;
            }
            SettingsAction::None => self.overlay = Some(Overlay::Settings(settings)),
        }
    }

    /// Esc on the settings screen: apply and save what changed, and say so.
    fn close_settings(&mut self, cx: &LoopCtx<'_>, settings: Box<SettingsView>, shown_mode: PermissionMode, shown_effort: &str) {
        let chosen_mode = settings.config.permission_mode;
        let chosen_effort = settings.config.thinking_effort.clone();
        let mut changes = Vec::new();
        if chosen_mode != shown_mode {
            changes.push(format!("mode ({})", chosen_mode.label()));
        }
        if settings.config.model != self.current_model {
            changes.push(format!("model ({})", settings.config.model));
        }
        if chosen_effort != shown_effort {
            changes.push(format!("thinking ({chosen_effort})"));
        }
        if changes.len() == 1 && chosen_mode != shown_mode {
            self.custom_placeholder = Some(format!("Permission mode set to: {}", chosen_mode.label()));
        } else if !changes.is_empty() {
            self.custom_placeholder = Some(format!("Settings updated: {}", changes.join(", ")));
        }
        self.suggested_prompt = None;

        // Every other setting is applied now; the release channel waits for
        // an answer, so declining costs the user nothing else they changed.
        let wanted_channel = settings.config.update_channel;
        let mut applied = persisted_from_view(&settings.config, &self.config, shown_mode, shown_effort);
        // Anything else changed (tips, toasts, goal limits...) is still worth
        // a word, and an older notice must not stay up as if it were the answer.
        let personality_changed = applied.personality != self.config.personality;
        if personality_changed {
            changes.push(format!("style ({})", applied.personality.summary()));
            self.custom_placeholder = Some(format!("Settings updated: {}", changes.join(", ")));
        }
        if changes.is_empty() && applied != self.config {
            self.custom_placeholder = Some("Settings saved".to_string());
        }
        if wanted_channel != self.config.update_channel {
            applied.update_channel = self.config.update_channel;
            self.custom_placeholder = None;
            self.ask_channel_switch(cx, wanted_channel);
        }
        self.config = applied;
        if personality_changed {
            self.apply_personality();
        }
        // The mode chosen here is the one to start in next time, also during
        // /goal, where it is the mode the run hands back when it ends.
        self.config.permission_mode = chosen_mode;
        let _ = self.config.save();
        cx.tools_arc.set_toolset_profile(self.config.toolset_profile);
        cx.tools_arc.set_web_enabled(self.config.web_tools);
        cx.source.0.set_max_retries(self.config.network_retries);
        if self.config.model != self.current_model {
            self.current_model = self.config.model.clone();
            cx.source.set_model(&self.current_model);
            cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
            cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
        }
        // During /goal the live mode/effort are the goal's; edits apply to
        // what the goal restores afterwards.
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
        self.refresh_welcome(cx.source, cx.perm.state().mode(), cx.mascot_mood);
    }

    /// Put up the yes-or-no card for moving to another release channel, and
    /// look up what that channel would install while it is up.
    pub(crate) fn ask_channel_switch(&mut self, cx: &LoopCtx<'_>, to: flashagent_core::config::UpdateChannel) {
        self.channel_switch = Some(ChannelSwitch { from: self.config.update_channel, to, target: ChannelTarget::Checking });
        let tx_ch = cx.channel_probe_tx.clone();
        tokio::spawn(async move {
            let target = match flashagent_svc::updater::newest_on_channel(to, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                Ok(Some(version)) => ChannelTarget::Version(version),
                Ok(None) => ChannelTarget::Empty,
                Err(_) => ChannelTarget::Unknown,
            };
            let _ = tx_ch.send(target);
        });
        self.renderer.request_reprint();
    }

    fn sampling_key(&mut self, mut view: SamplingView, code: KeyCode, mods: KeyModifiers) {
        match view.handle_key(code, mods) {
            SamplingAction::Close => {}
            SamplingAction::SaveAndClose => {
                view.apply_to_config(&mut self.config);
                let _ = self.config.save();
                self.notice("Sampling parameters updated");
            }
            SamplingAction::None => self.overlay = Some(Overlay::Sampling(view)),
        }
    }

    /// The memory screen owns every key while it is up: 'd' and 'e' are
    /// commands on the list and letters inside a note.
    fn memory_key(
        &mut self,
        cx: &LoopCtx<'_>,
        mut modal: flashagent_tui::memory_view::MemoryModal,
        code: KeyCode,
        mods: KeyModifiers,
    ) {
        use flashagent_tui::memory_view::MemoryAction;
        match modal.handle_key(code, mods) {
            MemoryAction::None => self.overlay = Some(Overlay::Memory(modal)),
            MemoryAction::Close => {}
            MemoryAction::Forget { name, scope } => {
                let cwd = std::env::current_dir().unwrap_or_default();
                let removed = flashagent_core::MemoryStore::for_scope(scope, &cwd)
                    .map(|store| store.remove(&name).unwrap_or(false))
                    .unwrap_or(false);
                self.notice(if removed { format!("[Forgot \"{name}\"]") } else { format!("[Could not forget \"{name}\"]") });
                self.overlay = Some(Overlay::Memory(open_memory_modal()));
            }
            MemoryAction::Summarize { .. } => {
                spawn_summary(cx.source.clone(), modal.rows.clone(), cx.tx.clone());
                self.overlay = Some(Overlay::Memory(modal));
            }
            MemoryAction::Tell { message } => {
                // Sent through the ordinary path, so it is an ordinary turn:
                // the model decides what to change and says so in the chat.
                self.input = message;
                let _ = cx.tx.send(UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE));
            }
        }
    }

    async fn mcp_key(&mut self, cx: &LoopCtx<'_>, mut modal: McpModal, code: KeyCode, mods: KeyModifiers) {
        match modal.handle_key(code, mods) {
            McpModalAction::Close => return,
            McpModalAction::Reload => {
                let mgr = cx.tools_arc.mcp_manager();
                let _ = mgr.reload().await;
                modal.servers = mgr.server_status_list().await;
                modal.status_message = Some("Reloaded MCP configurations".to_string());
            }
            McpModalAction::TestServer(name) => {
                let mgr = cx.tools_arc.mcp_manager();
                modal.status_message = Some(match mgr.test_server(&name).await {
                    Ok(report) => {
                        let ver = report.server_version.as_deref().unwrap_or("1.0.0");
                        let lat = format!("{:.1}ms", report.latency.as_secs_f64() * 1000.0);
                        format!("✔ {name} connected (v{ver}, {lat}, {} tools)", report.tools.len())
                    }
                    Err(e) => format!("✕ {name} failed: {e}"),
                });
                modal.servers = mgr.server_status_list().await;
            }
            McpModalAction::InstallMarketplace(id) => {
                if let Some(item) = flashagent_tools::mcp::find_marketplace_item(&id) {
                    let cfg = flashagent_tools::mcp::scaffold_config(item);
                    match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                        Ok(path) => {
                            modal.status_message = Some(format!("✔ Added {} to {}", item.name, path.display()));
                            let mgr = cx.tools_arc.mcp_manager();
                            let server_id = item.id.to_string();
                            tokio::spawn(async move {
                                let _ = mgr.reload().await;
                                let _ = mgr.start_server(&server_id).await;
                            });
                        }
                        Err(e) => modal.status_message = Some(format!("✕ Failed to install {id}: {e}")),
                    }
                }
            }
            McpModalAction::None => {}
        }
        self.overlay = Some(Overlay::Mcp(modal));
    }

    /// The list of saved sessions (/resume). Enter only records the pick: the
    /// switch happens at the top of the next loop turn, which owns the session
    /// id and the snapshot store.
    fn session_menu_key(&mut self, mut menu: SelectMenu<String>, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Enter => self.pending_resume = menu.selected_value().cloned(),
            KeyCode::Esc => {}
            _ => {
                navigate_menu(&mut menu, code, mods);
                self.overlay = Some(Overlay::Sessions(menu));
            }
        }
    }

    fn model_menu_key(&mut self, cx: &LoopCtx<'_>, mut menu: SelectMenu<String>, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Enter => {
                let Some(val) = menu.selected_value().cloned() else { return };
                self.current_model = val;
                self.config.model = self.current_model.clone();
                let _ = self.config.save();
                cx.source.set_model(&self.current_model);
                cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                if let Some(m) = cx.source.discovery().and_then(|d| d.models.into_iter().find(|m| m.id == self.current_model)) {
                    self.current_context = m.context_display();
                    // The effort the user picked survives the switch; a model
                    // that cannot reason just receives no thinking fields.
                    if self.current_effort.is_empty() {
                        self.current_effort = "auto".to_string();
                    }
                }
                self.refresh_welcome(cx.source, cx.perm.state().mode(), cx.mascot_mood);
                self.notice(format!("Switched active model to: {}", self.current_model));
            }
            KeyCode::Esc => {}
            _ => {
                navigate_menu(&mut menu, code, mods);
                self.overlay = Some(Overlay::Model(menu));
            }
        }
    }

    /// The /rewind confirmation card. Only Enter on "Yes, rewind" ever
    /// touches a file; everything else just closes the card.
    fn rewind_key(&mut self, cx: &mut LoopCtx<'_>, mut card: RewindConfirm, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::Left | KeyCode::Right => {
                card.toggle();
                self.overlay = Some(Overlay::Rewind(card));
            }
            KeyCode::Enter if card.confirm => self.commit_rewind(cx, card.target),
            _ => {}
        }
    }

    fn effort_menu_key(&mut self, cx: &LoopCtx<'_>, mut menu: SelectMenu<String>, code: KeyCode) {
        match code {
            KeyCode::Up => menu.up(),
            KeyCode::Down => menu.down(),
            KeyCode::Enter => {
                if let Some(val) = menu.selected_value().cloned() {
                    self.current_effort = val;
                    self.refresh_welcome(cx.source, cx.perm.state().mode(), cx.mascot_mood);
                    self.notice(format!("Thinking effort set to: {}", self.current_effort));
                }
                return;
            }
            KeyCode::Esc => return,
            _ => {}
        }
        self.overlay = Some(Overlay::Effort(menu));
    }

    /// PageUp/PageDown, Home/End and modified arrows move through the
    /// transcript; once scrolled back, plain arrows do too, and typing
    /// returns to the bottom.
    fn scroll_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Flow {
        let (_, height) = crossterm::terminal::size().unwrap_or((100, 24));
        let page = (height as usize / 2).max(5);
        let scrolled = self.renderer.scroll_offset > 0;
        let modified = mods.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL | KeyModifiers::ALT);
        match code {
            KeyCode::PageUp => self.renderer.scroll_up(page),
            KeyCode::PageDown => self.renderer.scroll_down(page),
            KeyCode::Home if scrolled => self.renderer.scroll_to_top(),
            KeyCode::End | KeyCode::Esc if scrolled => self.renderer.scroll_to_bottom(),
            KeyCode::Up if modified || scrolled => self.renderer.scroll_up(2),
            KeyCode::Down if modified || scrolled => self.renderer.scroll_down(2),
            KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter => {
                self.renderer.scroll_to_bottom();
                return Flow::Next;
            }
            _ => return Flow::Next,
        }
        Flow::Continue
    }

    pub(crate) fn open_model_menu(&mut self, source: &BackendSource) {
        match build_model_menu(source) {
            Some(mut menu) => {
                menu.select_by_value(&self.current_model);
                self.open_overlay(Overlay::Model(menu));
            }
            None => self.notice("[No models discovered from server]"),
        }
    }

    pub(crate) fn open_effort_menu(&mut self, source: &BackendSource) {
        let mut menu = build_effort_menu(source, &self.effort_memory, &self.current_model);
        menu.select_by_value(&self.current_effort);
        self.open_overlay(Overlay::Effort(menu));
    }

    pub(crate) async fn open_mcp(&mut self, cx: &LoopCtx<'_>, tab: McpViewTab) {
        let mgr = cx.tools_arc.mcp_manager();
        let paths = mgr.loaded_paths();
        let statuses = mgr.server_status_list().await;
        self.open_overlay(Overlay::Mcp(McpModal::new(paths, statuses, tab)));
    }
}

/// Arrows, pages and type-to-filter on a list menu.
fn navigate_menu(menu: &mut SelectMenu<String>, code: KeyCode, mods: KeyModifiers) {
    match code {
        KeyCode::Up => menu.up(),
        KeyCode::Down => menu.down(),
        KeyCode::PageUp => menu.page_up(),
        KeyCode::PageDown => menu.page_down(),
        KeyCode::Backspace => menu.pop_filter_char(),
        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
            menu.push_filter_char(c);
        }
        _ => {}
    }
}
