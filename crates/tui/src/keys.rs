use super::*;

impl App {
    /// A key the overlays did not claim: typing, editing the prompt, history,
    /// function keys and shortcuts, and Enter.
    pub(crate) async fn handle_key(&mut self, cx: &mut LoopCtx<'_>, code: KeyCode, mods: KeyModifiers) -> Flow {
        macro_rules! notice {
            ($text:expr) => {{
                self.custom_placeholder = Some(($text).to_string());
                self.suggested_prompt.take();
                self.renderer.request_reprint();
            }};
        }
        match code {
            KeyCode::Esc => {
                if self.running {
                    // Cooperative: the loop answers pending tool calls,
                    // keeps partial text and returns its history via
                    // Finished, so model memory matches the screen.
                    if self.cancel_requested.is_none() {
                        cx.cancel.store(true, Ordering::Relaxed);
                        self.cancel_requested = Some(std::time::Instant::now());
                        self.turn_phase = TurnPhase::Stopping;
                        self.active_steer_tx = None;
                        self.suggested_prompt = None;
                        self.custom_placeholder = Some("Interrupting...".to_string());
                    }
                    self.renderer.request_reprint();
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.autocomplete_idx = 0;
                    self.history_index = None;
                    if self.latest_suggestion.is_some() {
                        self.suggested_prompt = self.latest_suggestion.clone();
                    }
                    self.renderer.request_reprint();
                } else if self.mcp_modal.is_some() {
                    self.mcp_modal = None;
                    self.renderer.request_reprint();
                } else if self.suggested_prompt.is_some() || self.custom_placeholder.is_some() {
                    self.suggested_prompt = None;
                    self.latest_suggestion = None;
                    self.custom_placeholder = None;
                    self.renderer.request_reprint();
                } else if !worth_saving(&self.history) {
                    // Nothing was said, so nothing is saved and there is no
                    // session to come back to: the question protects nothing.
                    return Flow::Quit;
                } else {
                    self.quit_confirm = true;
                    self.renderer.request_reprint();
                }
            }
            // F1: toggle context modal
            KeyCode::F(1) => {
                if self.context_modal.is_some() {
                    self.context_modal = None;
                } else {
                    self.effort_menu = None;
                    self.model_menu = None;
                    self.settings_view = None;
                    self.sampling_view = None;
                    self.mcp_modal = None;
                    self.context_modal = Some(ContextModal::new(self.context_usage.clone()));
                }
                self.renderer.request_reprint();
            }

            // Ctrl+C / Ctrl+Shift+C (handles both Latin and alternate physical keycodes)
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                if mods.contains(KeyModifiers::CONTROL) =>
            {
                if self.running {
                    // Cooperative: the loop answers pending tool calls,
                    // keeps partial text and returns its history via
                    // Finished, so model memory matches the screen.
                    if self.cancel_requested.is_none() {
                        cx.cancel.store(true, Ordering::Relaxed);
                        self.cancel_requested = Some(std::time::Instant::now());
                        self.turn_phase = TurnPhase::Stopping;
                        self.active_steer_tx = None;
                        self.suggested_prompt = None;
                        self.custom_placeholder = Some("Interrupting...".to_string());
                    }
                    self.renderer.request_reprint();
                } else {
                    let now = std::time::Instant::now();
                    let is_double_tap = self.last_ctrl_c.map(|t| now.duration_since(t).as_millis() < 1200).unwrap_or(false);
                    self.last_ctrl_c = Some(now);

                    if !self.input.is_empty() {
                        flashagent_tui::clipboard::set_clipboard_text(&self.input);
                        self.copy_toast = Some(("Copied input to clipboard".to_string(), now));
                        self.renderer.request_reprint();
                    } else if let Some(text) = self.chat.last_assistant_text() {
                        if is_double_tap {
                            return Flow::Quit;
                        }
                        flashagent_tui::clipboard::set_clipboard_text(&text);
                        self.copy_toast = Some(("Copied assistant response (press Ctrl+C again to exit)".to_string(), now));
                        self.renderer.request_reprint();
                    } else {
                        return Flow::Quit;
                    }
                }
            }

            // Ctrl+V / Ctrl+Shift+V: paste from clipboard (supports alternative keyboard layouts)
            KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\u{043c}') | KeyCode::Char('\u{041c}')
                if mods.contains(KeyModifiers::CONTROL) =>
            {
                // A screenshot on the clipboard is what the user
                // means by paste far more often than the file path
                // of one, so it is looked for first.
                if let Some(image) = flashagent_tui::clipboard::get_clipboard_image() {
                    maybe_measure_image_cost(
                        &self.image_costs,
                        &mut self.image_cost_probe,
                        &self.current_model,
                        cx.source,
                        cx.tx,
                    );
                    let att = Attachment::from_clipboard(image);
                    let label = att.label();
                    self.attachments.push(att);
                    if !model_sees_images(cx.source, &self.current_model) {
                        self.background = Some(BackgroundNotice::sticky(format!(
                            "{label} attached · {} cannot see images — press F3 for one that can", self.current_model
                        )));
                    } else {
                        self.background = Some(BackgroundNotice::fading(
                            format!("{label} attached · Ctrl+Z removes it"),
                            8,
                        ));
                    }
                    self.renderer.request_reprint();
                } else if let Some(text) = flashagent_tui::clipboard::get_clipboard_text() {
                    let sanitized = text.replace("\r\n", " ").replace(['\n', '\r'], " ");
                    if !sanitized.is_empty() {
                        self.input.push_str(&sanitized);
                        self.history_index = None;
                        self.autocomplete_idx = 0;
                        self.renderer.request_reprint();
                    }
                }
            }

            // Ctrl+Z: take back the last picture attached.
            KeyCode::Char('z') | KeyCode::Char('Z') | KeyCode::Char('\u{044f}') | KeyCode::Char('\u{042f}')
                if mods.contains(KeyModifiers::CONTROL) && !self.attachments.is_empty() =>
            {
                if let Some(removed) = self.attachments.pop() {
                    self.background = Some(BackgroundNotice::fading(
                        format!("{} removed", removed.label()),
                        5,
                    ));
                }
                self.renderer.request_reprint();
            }

            // Ctrl+D: exit on empty input when idle
            KeyCode::Char('d') | KeyCode::Char('D')
                if mods.contains(KeyModifiers::CONTROL) && self.input.is_empty() && !self.running =>
            {
                return Flow::Quit;
            }

            // Ctrl+R / Ctrl+Shift+R: regenerate last response from scratch (supports alternative keyboard layouts)
            KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Char('\u{043a}') | KeyCode::Char('\u{041a}')
                if mods.contains(KeyModifiers::CONTROL) =>
            {
                if !self.running
                    && cx.gate.pending().is_none()
                    && cx.question_gate.pending().is_none()
                    && self.effort_menu.is_none()
                    && self.model_menu.is_none()
                    && self.settings_view.is_none()
                    && self.sampling_view.is_none()
                    && self.context_modal.is_none()
                {
                    if let Some(user_idx) = self.history.iter().rposition(|m| m.role == flashagent_llm::Role::User) {
                        // Asking for the same answer again is the
                        // user saying the last one was not good
                        // enough — the one signal that the model was
                        // given too little room to think.
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
                        let now = std::time::Instant::now();
                        self.copy_toast = Some(("No previous turn to regenerate".to_string(), now));
                        self.renderer.request_reprint();
                    }
                }
            }

            // F2: cycle reasoning expansion mode (none -> last -> all -> none)
            KeyCode::F(2) => {
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
            }

            // ALT + O: expand / collapse ALL thinking blocks permanently (supports alternative keyboard layouts)
            KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('\u{0449}') | KeyCode::Char('\u{0429}')
                if mods.contains(KeyModifiers::ALT) =>
            {
                self.all_expanded = !self.all_expanded;
                if self.all_expanded {
                    self.last_expanded = false;
                }
                self.renderer.request_reprint();
            }

            // CTRL + O: expand / collapse LAST thinking block temporarily (supports alternative keyboard layouts)
            KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('\u{0449}') | KeyCode::Char('\u{0429}')
                if mods.contains(KeyModifiers::CONTROL) =>
            {
                self.last_expanded = !self.last_expanded;
                if self.last_expanded {
                    self.all_expanded = false;
                }
                self.renderer.request_reprint();
            }

            // CTRL + E: launch external editor on current input buffer
            KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Char('\u{0443}') | KeyCode::Char('\u{0423}')
                if mods.contains(KeyModifiers::CONTROL) && !self.running =>
            {
                match open_in_external_editor(&self.input, &self.config.external_editor) {
                    Ok(edited) => {
                        self.input = edited;
                        self.autocomplete_idx = 0;
                    }
                    Err(err) => {
                        notice!(&format!("Failed to launch external editor: {err}"));
                    }
                }
                self.renderer.request_reprint();
            }

            // CTRL + U: download & apply pending update or check for updates
            KeyCode::Char('u') | KeyCode::Char('U') | KeyCode::Char('\u{0433}') | KeyCode::Char('\u{0413}')
                if mods.contains(KeyModifiers::CONTROL) =>
            {
                if let Some((target_ver, asset_name, download_url, checksums_url)) = self.pending_update.clone() {
                    self.background = Some(BackgroundNotice::sticky(format!(
                        "{UPDATE_LINE_PREFIX}{target_ver} \u{b7} starting download..."
                    )));
                    self.renderer.request_reprint();
                    let update_tx_clone = cx.update_tx.clone();
                    tokio::spawn(async move {
                        let progress_tx = update_tx_clone.clone();
                        let ver_for_progress = target_ver.clone();
                        let result = flashagent_svc::updater::download_and_apply_with_progress(
                            &download_url,
                            &asset_name,
                            checksums_url.as_deref(),
                            move |stage| {
                                let _ = progress_tx.send(UpdateNotice::Progress {
                                    version: ver_for_progress.clone(),
                                    stage,
                                });
                            },
                        )
                        .await;
                        let notice = match result {
                            Ok(_) => UpdateNotice::Ready { version: target_ver },
                            Err(e) => UpdateNotice::Failed { error: e.to_string() },
                        };
                        let _ = update_tx_clone.send(notice);
                    });
                } else if !flashagent_svc::updater::is_dev_mode() {
                    self.background = Some(BackgroundNotice::fading(
                        format!(
                            "{UPDATE_LINE_PREFIX}\u{b7} checking the {} channel...",
                            self.config.update_channel.label()
                        ),
                        30,
                    ));
                    self.renderer.request_reprint();
                    let update_tx_clone = cx.update_tx.clone();
                    let ch = self.config.update_channel;
                    tokio::spawn(async move {
                        match flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                            // Asked for by hand: go straight on to the
                            // download instead of making the user press
                            // Ctrl+U a second time.
                            Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                let progress_tx = update_tx_clone.clone();
                                let ver_for_progress = target.clone();
                                let result = flashagent_svc::updater::download_and_apply_with_progress(
                                    &download_url,
                                    &asset_name,
                                    checksums_url.as_deref(),
                                    move |stage| {
                                        let _ = progress_tx.send(UpdateNotice::Progress {
                                            version: ver_for_progress.clone(),
                                            stage,
                                        });
                                    },
                                )
                                .await;
                                let _ = update_tx_clone.send(match result {
                                    Ok(_) => UpdateNotice::Ready { version: target },
                                    Err(e) => UpdateNotice::Failed { error: e.to_string() },
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
                } else {
                    self.background = Some(BackgroundNotice::fading("Auto-updater is disabled in dev mode", 6));
                    self.renderer.request_reprint();
                }
            }

            // F4 or CTRL + T or ALT + T: open Thinking Effort menu (supports alternative keyboard layouts)
            KeyCode::F(4)
            | KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Char('\u{0435}') | KeyCode::Char('\u{0415}')
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(4)) =>
            {
                let mut menu = build_effort_menu(cx.source, &self.effort_memory, &self.current_model);
                menu.select_by_value(&self.current_effort);
                self.effort_menu = Some(menu);
            }

            // F3 or CTRL + M or ALT + M: open Model menu (supports alternative keyboard layouts)
            KeyCode::F(3)
            | KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Char('\u{044c}') | KeyCode::Char('\u{042c}')
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(3)) =>
            {
                if let Some(mut menu) = build_model_menu(cx.source) {
                    menu.select_by_value(&self.current_model);
                    self.model_menu = Some(menu);
                } else {
                    notice!("[No models discovered from server]");
                }
            }

            // F5: open Sampling Parameters menu
            KeyCode::F(5) => {
                if self.sampling_view.is_some() {
                    self.sampling_view = None;
                } else {
                    self.effort_menu = None;
                    self.model_menu = None;
                    self.settings_view = None;
                    self.context_modal = None;
                    self.mcp_modal = None;
                    self.sampling_view = Some(SamplingView::new(&self.config));
                }
                self.renderer.request_reprint();
            }

            // Mode cycling with Shift+Tab (both KeyCode::BackTab and Tab+Shift)
            KeyCode::BackTab | KeyCode::Tab if matches!(code, KeyCode::BackTab) || mods.contains(KeyModifiers::SHIFT) => {
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
            }
            // Tab on empty input: toggle settings tab
            KeyCode::Tab if self.input.is_empty() && cx.gate.pending().is_none() && self.effort_menu.is_none() && self.model_menu.is_none() && self.sampling_view.is_none() && self.mcp_modal.is_none() => {
                if self.settings_view.is_some() {
                    self.settings_view = None;
                } else {
                    self.mcp_modal = None;
                    let runtime_mode = self.goal_state.as_ref().map_or(cx.perm.state().mode(), |g| g.mode);
                    let effort = self.goal_state.as_ref().map_or(self.current_effort.as_str(), |g| g.effort.as_str());
                    self.settings_view = Some(settings_for_runtime(&self.config, runtime_mode, effort, &self.current_model, &self.available_models, self.context_usage.total_capacity));
                }
                self.renderer.request_reprint();
            }
            // Tab: complete autocomplete suggestion if input starts with `/`, or toggle approval choice when pending
            KeyCode::Tab if cx.gate.pending().is_some() => {
                self.confirm_select.toggle();
            }
            KeyCode::Tab if self.input.starts_with('/') => {
                if let Some(ac) = AutocompletePopup::for_input(&self.input, std::path::Path::new("."), self.autocomplete_idx) {
                    self.input = ac.complete_input(&self.input);
                    self.autocomplete_idx = 0;
                }
            }
            // Arrow navigation for approval card, autocomplete popup, and prompt history
            KeyCode::Left => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.left();
                }
            }
            KeyCode::Right => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.right();
                } else if !self.running && self.input.is_empty() {
                    if let Some(sug) = self.suggested_prompt.take() {
                        self.latest_suggestion = None;
                        self.input = sug;
                        self.renderer.request_reprint();
                    }
                }
            }
            KeyCode::Up => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.toggle();
                } else if !self.running && self.input.starts_with('/') {
                    if let Some(ac) = AutocompletePopup::for_input(&self.input, std::path::Path::new("."), self.autocomplete_idx) {
                        if self.autocomplete_idx == 0 {
                             self.autocomplete_idx = ac.items.len().saturating_sub(1);
                        } else {
                             self.autocomplete_idx -= 1;
                        }
                    }
                } else if !self.running && !self.input_history.is_empty() {
                    match self.history_index {
                        None => {
                            self.current_draft = self.input.clone();
                            let idx = self.input_history.len() - 1;
                            self.history_index = Some(idx);
                            self.input = self.input_history[idx].clone();
                        }
                        Some(idx) if idx > 0 => {
                            let new_idx = idx - 1;
                            self.history_index = Some(new_idx);
                            self.input = self.input_history[new_idx].clone();
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Down => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.toggle();
                } else if !self.running && self.input.starts_with('/') {
                    if let Some(ac) = AutocompletePopup::for_input(&self.input, std::path::Path::new("."), self.autocomplete_idx) {
                        self.autocomplete_idx = (self.autocomplete_idx + 1) % ac.items.len();
                    }
                } else if !self.running {
                    if let Some(idx) = self.history_index {
                        if idx + 1 < self.input_history.len() {
                            let new_idx = idx + 1;
                            self.history_index = Some(new_idx);
                            self.input = self.input_history[new_idx].clone();
                        } else {
                            self.history_index = None;
                            self.input = std::mem::take(&mut self.current_draft);
                        }
                    }
                }
            }
            KeyCode::Backspace if cx.gate.pending().is_none() => {
                self.input.pop();
                self.history_index = None;
                self.autocomplete_idx = 0;
                if self.input.is_empty() && self.latest_suggestion.is_some() {
                    self.suggested_prompt = self.latest_suggestion.clone();
                }
            }
            KeyCode::Enter => {
                self.custom_placeholder = None;
                self.suggested_prompt = None;
                self.latest_suggestion = None;
                if cx.gate.pending().is_some() {
                    cx.gate.respond(self.confirm_select.decision());
                    self.confirm_select = ConfirmSelect::new();
                } else if !self.input.is_empty() && self.running {
                    if let Some(ref steer_tx) = self.active_steer_tx {
                        let text = std::mem::take(&mut self.input);
                        if self.input_history.last() != Some(&text) {
                            self.input_history.push(text.clone());
                        }
                        self.history_index = None;
                        self.current_draft.clear();
                        let _ = steer_tx.send(text);
                        self.renderer.request_reprint();
                    }
                } else if !self.input.is_empty() && !self.running {
                    match self.submit_input(cx).await {
                        Flow::Continue => return Flow::Continue,
                        Flow::Quit => return Flow::Quit,
                        Flow::Next => {}
                    }
                }
            }
            KeyCode::Char(c)
                if cx.gate.pending().is_none()
                    && !mods.contains(KeyModifiers::CONTROL)
                    && !mods.contains(KeyModifiers::ALT) =>
            {
                self.input.push(c);
                self.history_index = None;
                self.autocomplete_idx = 0;
            }
            _ => {}
        }
        Flow::Next
    }
}
