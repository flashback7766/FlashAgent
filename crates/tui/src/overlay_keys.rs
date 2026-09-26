use super::*;

/// Latin or Cyrillic.
fn is_yes(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('\u{043d}') | KeyCode::Char('\u{041d}') | KeyCode::Enter
    )
}

/// Latin or Cyrillic layout.
pub(crate) fn is_ctrl_c(code: KeyCode, mods: KeyModifiers) -> bool {
    matches!(code, KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}'))
        && mods.contains(KeyModifiers::CONTROL)
}

impl App {
    /// Cards are asked in drawing order, so the key goes to the one on top: an
    /// approval over open settings takes Enter. `Flow::Next` means none claimed it.
    pub(crate) async fn handle_overlay_key(&mut self, cx: &mut LoopCtx<'_>, code: KeyCode, mods: KeyModifiers) -> Flow {
        if cx.gate.pending().is_some() {
            // The conversation above stays readable while the card waits.
            if matches!(code, KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home | KeyCode::End) {
                self.scroll_key(code, mods);
            } else {
                self.approval_key(cx, code, mods);
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        // Yes closes the app; the uninstaller then runs in the restored terminal.
        if self.uninstall_confirm {
            self.uninstall_confirm = false;
            if is_yes(code) {
                UNINSTALL_AFTER_EXIT.store(true, Ordering::SeqCst);
                return Flow::Quit;
            }
            self.renderer.request_reprint();
            return Flow::Continue;
        }

        // Owns the keyboard: typing past a yes-or-no about replacing the binary would
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

        // Taken out while its key is handled, put back unless the key closed it or
        // opened another.
        if let Some(overlay) = self.overlay.take() {
            match overlay {
                Overlay::Settings(view) => self.settings_key(cx, view, code, mods).await,
                Overlay::Sampling(view) => self.sampling_key(view, code, mods),
                Overlay::Memory(modal) => self.memory_key(cx, modal, code, mods),
                Overlay::Context(modal) => {
                    if !matches!(code, KeyCode::F(1) | KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q')) {
                        self.overlay = Some(Overlay::Context(modal));
                    }
                }
                Overlay::Mcp(modal) => self.mcp_key(cx, modal, code, mods).await,
                Overlay::Tasks(modal) => self.tasks_key(cx, modal, code, mods),
                Overlay::Sessions(menu) => self.session_menu_key(menu, code, mods),
                Overlay::Model(menu) => self.model_menu_key(cx, menu, code, mods),
                Overlay::Provider(menu) => self.provider_menu_key(cx, menu, code, mods).await,
                Overlay::Providers(view) => self.providers_key(cx, view, code, mods).await,
                Overlay::Rewind(card) => self.rewind_key(cx, card, code),
                Overlay::Effort(menu) => self.effort_menu_key(cx, menu, code),
                Overlay::Palette(menu) => {
                    let flow = self.palette_key(cx, menu, code, mods).await;
                    self.renderer.request_reprint();
                    return flow;
                }
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
                self.allow_always(cx);
            }
            KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\u{043c}') | KeyCode::Char('\u{041c}') => self.show_whole_change(cx),
            KeyCode::Enter if self.confirm_select.choice() == ConfirmChoice::Always => self.allow_always(cx),
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

    /// The whole diff, into the conversation above the card, once per call:
    /// the card has room for only part of a long change.
    fn show_whole_change(&mut self, cx: &LoopCtx<'_>) {
        let Some(req) = cx.gate.pending() else { return };
        let Some(diff) = req.diff.as_deref() else { return };
        if self.whole_change_shown.as_deref() == Some(req.args_json.as_str()) {
            return;
        }
        let (width, _) = crossterm::terminal::size().unwrap_or((100, 24));
        self.chat.push_line(LineKind::System, format!("  \x1b[1;38;2;225;175;95mThe whole change\x1b[0m \x1b[38;2;135;130;125m· {}\x1b[0m", req.tool));
        for row in crate::render::diff_rows(diff, (width as usize).saturating_sub(2)) {
            self.chat.push_line(LineKind::System, row);
        }
        self.whole_change_shown = Some(req.args_json.clone());
        self.renderer.scroll_to_bottom();
    }

    fn allow_always(&mut self, cx: &LoopCtx<'_>) {
        if let Some(req) = cx.gate.pending() {
            let rules = cx.perm.state().allow_always(&req);
            if rules.is_empty() {
                self.notice("Allowed once · this command is too broad to allow always");
            } else {
                self.notice(format!("Always allowed this session: {}", rules.join(", ")));
            }
        }
        cx.gate.respond(Decision::Allow);
        self.confirm_select = ConfirmSelect::new();
    }

    fn channel_switch_key(&mut self, cx: &LoopCtx<'_>, sw: ChannelSwitch, code: KeyCode) {
        if is_yes(code) {
            self.config.update_channel = sw.to;
            self.save_config();
            if cx.channel_watch_tx.receiver_count() > 0 {
                let _ = cx.channel_watch_tx.send(sw.to);
            }
            if let Some(view) = self.settings_view_mut() {
                view.config.update_channel = sw.to;
            }
            self.background = Some(BackgroundNotice::sticky(format!(
                "Release channel is now {} \u{b7} /update moves to it",
                sw.to.label()
            )));
        } else {
            // Other changes stay applied; only this one is put back.
            if let Some(view) = self.settings_view_mut() {
                view.config.update_channel = sw.from;
            }
            self.background = Some(BackgroundNotice::fading(format!("Still on the {} channel", sw.from.label()), 5));
        }
    }

    fn question_key(&mut self, cx: &LoopCtx<'_>, req: &flashagent_tui::QuestionRequest, code: KeyCode, mods: KeyModifiers) {
        if is_ctrl_c(code, mods) {
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
                } else if let (Some(opts), false) = (&req.options, c.is_ascii_digit() || c.is_whitespace()) {
                    // Typing starts an answer of one's own, with no key to open it first. A
                    // path pasted into the card (keys, in a Windows console) is typed too,
                    // so the digits in it no longer pick options.
                    state.selected_index = opts.len();
                    state.is_writing = true;
                    state.write_in_text = c.to_string();
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
        // Live session values.
        let shown_mode = self.goal_state.as_ref().map_or(cx.perm.state().mode(), |g| g.mode);
        let shown_effort = self.goal_state.as_ref().map_or(self.current_effort.clone(), |g| g.effort.clone());
        match settings.handle_key(code, mods) {
            SettingsAction::Close => self.close_settings(cx, settings, shown_mode, &shown_effort),
            SettingsAction::OpenProviders => {
                self.keep_settings(cx, &settings.config, shown_mode, &shown_effort);
                self.open_providers_view();
            }
            SettingsAction::RunToolTest => {
                settings.tool_test_status = Some(format!("Probing {}…", self.current_model));
                let source_bg = cx.source.clone();
                let tx_bg = cx.tx.clone();
                tokio::spawn(async move {
                    let verdict = run_tool_call_probe(&source_bg).await;
                    let _ = tx_bg.send(UiEvent::ToolTestResult(verdict));
                });
                self.overlay = Some(Overlay::Settings(settings));
            }
            SettingsAction::OpenModelMenu => {
                self.keep_settings(cx, &settings.config, shown_mode, &shown_effort);
                self.open_model_menu(cx.source);
            }
            SettingsAction::OpenEffortMenu => {
                self.keep_settings(cx, &settings.config, shown_mode, &shown_effort);
                self.open_effort_menu(cx.source);
            }
            SettingsAction::OpenWizard => {
                self.keep_settings(cx, &settings.config, shown_mode, &shown_effort);
                self.run_setup_in_app(cx, false).await;
            }
            SettingsAction::OpenSamplingMenu => {
                self.keep_settings(cx, &settings.config, shown_mode, &shown_effort);
                self.overlay = Some(Overlay::Sampling(SamplingView::new(&self.config)));
            }
            SettingsAction::CheckUpdatesNow => {
                if flashagent_svc::updater::is_dev_mode() {
                    settings.update_check_status = Some("Dev mode: updates disabled (source build)".into());
                } else {
                    settings.update_check_status = Some("Checking GitHub releases…".into());
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
                self.keep_settings(cx, &settings.config, shown_mode, &shown_effort);
                self.open_mcp(cx, McpViewTab::Overview).await;
            }
            SettingsAction::None => self.overlay = Some(Overlay::Settings(settings)),
        }
    }

    /// Settings left for one of its sub-screens: what was changed is kept.
    fn keep_settings(&mut self, cx: &LoopCtx<'_>, view: &AppConfig, shown_mode: PermissionMode, shown_effort: &str) {
        let mut applied = persisted_from_view(view, &self.config, shown_mode, shown_effort);
        let switching = self.provider_picked(&applied);
        if switching && self.running {
            self.keep_provider(&mut applied);
        }
        self.config = applied;
        self.save_config();
        if switching && !self.running {
            self.connect_active(cx);
        }
    }

    /// Settings picked another provider than the one in use.
    fn provider_picked(&self, applied: &AppConfig) -> bool {
        applied.active_profile().name != self.config.active_profile().name || (applied.run_provider.is_none() && self.config.run_provider.is_some())
    }

    /// A switch waits for the running answer; the rest of what was changed stays.
    fn keep_provider(&mut self, applied: &mut AppConfig) {
        applied.active_provider = self.config.active_provider.clone();
        applied.run_provider = self.config.run_provider.clone();
        self.refuse_switch_while_busy();
    }

    fn close_settings(&mut self, cx: &LoopCtx<'_>, settings: Box<SettingsView>, shown_mode: PermissionMode, shown_effort: &str) {
        let chosen_mode = settings.config.permission_mode;
        let chosen_effort = settings.config.thinking_effort.clone();
        let mut changes = Vec::new();
        let switching = self.provider_picked(&settings.config);
        if chosen_mode != shown_mode {
            changes.push(format!("mode ({})", chosen_mode.label()));
        }
        if switching {
            changes.push(format!("provider ({})", settings.config.active_profile().name));
        } else if settings.config.active_profile().model != self.current_model {
            changes.push(format!("model ({})", settings.config.active_profile().model));
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

        // The channel waits for an answer, so declining costs nothing else.
        let wanted_channel = settings.config.update_channel;
        let mut applied = persisted_from_view(&settings.config, &self.config, shown_mode, shown_effort);
        // Other changes still get a word, replacing any older notice.
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
        if switching && self.running {
            self.keep_provider(&mut applied);
        }
        self.config = applied;
        if personality_changed {
            self.apply_personality();
        }
        // Also during /goal, where it is the mode the run hands back.
        self.config.permission_mode = chosen_mode;
        self.save_config();
        cx.tools_arc.set_toolset_profile(self.config.toolset_profile);
        cx.tools_arc.set_web_enabled(self.config.web_tools);
        cx.source.0.set_max_retries(self.config.network_retries);
        cx.source.0.set_user_sampling(self.config.sampling_preset == flashagent_core::config::SamplingPreset::Custom);
        if switching && !self.running {
            self.connect_active(cx);
        } else if !switching && !self.config.active_profile().model.is_empty() && self.config.active_profile().model != self.current_model {
            let model = self.config.active_profile().model.clone();
            self.switch_model(cx, &model);
        }
        // During /goal the live mode/effort are the goal's; edits apply to what the
        // goal restores afterwards.
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
        self.refresh_welcome(cx.source, cx.mascot_mood);
    }

    /// Looks up what the channel would install while the card is up.
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
                self.save_config();
                self.notice("Sampling parameters updated");
            }
            SamplingAction::None => self.overlay = Some(Overlay::Sampling(view)),
        }
    }

    /// Owns every key: 'd' and 'e' are list commands and letters in a note.
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
                self.notice(if removed { format!("Forgot \"{name}\"") } else { format!("Could not forget \"{name}\"") });
                self.overlay = Some(Overlay::Memory(open_memory_modal()));
            }
            MemoryAction::Summarize { .. } => {
                spawn_summary(cx.source.clone(), modal.rows.clone(), cx.tx.clone());
                self.overlay = Some(Overlay::Memory(modal));
            }
            MemoryAction::Tell { message } => {
                // An ordinary turn: the model decides what to change and says so.
                self.input.set(message);
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
                        format!("√ {name} connected (v{ver}, {lat}, {} tools)", report.tools.len())
                    }
                    Err(e) => format!("× {name} failed: {e}"),
                });
                modal.servers = mgr.server_status_list().await;
            }
            McpModalAction::InstallMarketplace(id) => {
                if let Some(item) = flashagent_tools::mcp::find_marketplace_item(&id) {
                    let cfg = flashagent_tools::mcp::scaffold_config(item);
                    match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                        Ok(path) => {
                            modal.status_message = Some(format!("√ Added {} to {}", item.name, path.display()));
                            let mgr = cx.tools_arc.mcp_manager();
                            let server_id = item.id.to_string();
                            tokio::spawn(async move {
                                let _ = mgr.reload().await;
                                let _ = mgr.start_server(&server_id).await;
                            });
                        }
                        Err(e) => modal.status_message = Some(format!("× Failed to install {id}: {e}")),
                    }
                }
            }
            McpModalAction::None => {}
        }
        self.overlay = Some(Overlay::Mcp(modal));
    }

    /// Enter only records the pick; the switch happens at the top of the next loop
    /// turn, which owns the session id and snapshot store.
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

    /// Everything that follows the model, at once: the next prompt may go out
    /// before the idle poll catches up (it never runs during a turn), and a
    /// 128K budget on an 8K model overflows the server. The new prefix is
    /// warmed while the user types.
    pub(crate) fn switch_model(&mut self, cx: &LoopCtx<'_>, model: &str) {
        self.current_model = model.to_string();
        cx.source.set_model(&self.current_model);
        cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
        cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
        if let Some(m) = cx.source.discovery().and_then(|d| d.models.into_iter().find(|m| m.id == self.current_model)) {
            self.current_context = m.context_display();
            if let Some(len) = m.context_length.or(m.max_context_length) {
                self.context_usage.total_capacity = len.max(1024);
                cx.tools_arc.set_context_window(Some(len));
            }
            // The picked effort survives the switch; a non-reasoning model just gets no
            // thinking fields.
            if self.current_effort.is_empty() {
                self.current_effort = "auto".to_string();
            }
        }
        if !self.running {
            self.sync_system_prompt();
        }
        update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
        self.warm_prompt_cache(cx.source, cx.perm, cx.memory_block);
    }

    fn model_menu_key(&mut self, cx: &LoopCtx<'_>, mut menu: SelectMenu<String>, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Enter => {
                let Some(val) = menu.selected_value().cloned() else { return };
                // The menu lists the connected server's models; during a switch they
                // are not the active provider's.
                if self.on_active_provider(cx.source) && self.provider_switch.is_none() {
                    self.config.active_profile_mut().model = val.clone();
                    self.save_config();
                }
                self.switch_model(cx, &val);
                self.refresh_welcome(cx.source, cx.mascot_mood);
                self.notice(format!("Model: {}", self.current_model));
            }
            KeyCode::Esc => {}
            _ => {
                navigate_menu(&mut menu, code, mods);
                self.overlay = Some(Overlay::Model(menu));
            }
        }
    }

    /// Only Enter on "Yes, rewind" touches a file.
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
                    self.refresh_welcome(cx.source, cx.mascot_mood);
                    self.notice(format!("Thinking effort set to: {}", self.current_effort));
                }
                return;
            }
            KeyCode::Esc => return,
            _ => {}
        }
        self.overlay = Some(Overlay::Effort(menu));
    }

    /// Typing narrows, Enter runs the command; one that needs an argument is put
    /// in the prompt to be finished. A draft in the prompt survives.
    async fn palette_key(&mut self, cx: &mut LoopCtx<'_>, mut menu: SelectMenu<String>, code: KeyCode, mods: KeyModifiers) -> Flow {
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => return Flow::Continue,
            KeyCode::Char(c) if ctrl && matches!(c, 'k' | 'K' | '\u{043b}' | '\u{041b}') => return Flow::Continue,
            KeyCode::Up => menu.up(),
            KeyCode::Down | KeyCode::Tab => menu.down(),
            KeyCode::PageUp => menu.page_up(),
            KeyCode::PageDown => menu.page_down(),
            KeyCode::Backspace => menu.pop_filter_char(),
            KeyCode::Char(c) if !ctrl && !mods.contains(KeyModifiers::ALT) => menu.push_filter_char(c),
            KeyCode::Enter => {
                let Some(command) = menu.selected_value().cloned().filter(|_| !menu.filtered_indices().is_empty()) else {
                    self.overlay = Some(Overlay::Palette(menu));
                    return Flow::Continue;
                };
                if command.ends_with(' ') {
                    self.input.set(command);
                    return Flow::Continue;
                }
                let draft = self.input.to_string();
                self.input.set(command);
                let flow = self.submit_input(cx).await;
                // The command took the prompt; what was being written comes back.
                if self.input.is_empty() {
                    self.input.set(draft);
                }
                return flow;
            }
            _ => {}
        }
        self.overlay = Some(Overlay::Palette(menu));
        Flow::Continue
    }

    /// Once scrolled back, plain arrows scroll too. The composer stays on screen,
    /// so typing keeps the place; sending returns to the bottom.
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
            KeyCode::Enter => {
                self.renderer.scroll_to_bottom();
                return Flow::Next;
            }
            _ => return Flow::Next,
        }
        Flow::Continue
    }

    pub(crate) fn open_model_menu(&mut self, source: &BackendSource) {
        if let Some(switch) = &self.provider_switch {
            let name = switch.name.clone();
            self.notice(format!("Still connecting to {name}; its models are listed once it answers"));
            return;
        }
        match build_model_menu(source, &self.config.active_profile().name) {
            Some(mut menu) => {
                menu.select_by_value(&self.current_model);
                self.open_overlay(Overlay::Model(menu));
            }
            None => self.notice("No models discovered from server"),
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

pub(crate) fn navigate_menu(menu: &mut SelectMenu<String>, code: KeyCode, mods: KeyModifiers) {
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
