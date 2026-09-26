use super::*;

/// Checking while a look may still answer; offline once one got nothing
/// (after a moment, so the face does not flash) or when none has answered
/// for half a minute.
pub(crate) fn server_mood(switching: bool, answered: bool, silent: bool, since_start: std::time::Duration) -> MascotMood {
    if switching {
        MascotMood::Checking
    } else if answered {
        MascotMood::Happy
    } else if (silent && since_start >= std::time::Duration::from_secs(2)) || since_start >= std::time::Duration::from_secs(30) {
        MascotMood::Offline
    } else {
        MascotMood::Checking
    }
}

impl App {
    /// One event from the terminal, the agent loop or a background task.
    pub(crate) async fn on_ui_event(&mut self, cx: &mut LoopCtx<'_>, ev: UiEvent) -> Flow {
        match ev {
            UiEvent::BackgroundRecap { turn_id, recap, suggestion } => self.on_recap(turn_id, &recap, suggestion),
            UiEvent::MemorySummary(result) => self.on_memory_summary(result),
            UiEvent::ImageCost { model, per_pixel, fixed } => {
                self.image_costs.set(&model, flashagent_tui::image_cost::ImageCost { per_pixel, fixed });
                self.image_costs.save();
                self.image_cost_probe = None;
                self.renderer.request_reprint();
            }
            UiEvent::ToolTestResult(verdict) => {
                match self.settings_view_mut() {
                    Some(s) => s.tool_test_status = Some(verdict),
                    None => self.notice(format!("Tool test: {verdict}")),
                }
                self.renderer.request_reprint();
            }
            UiEvent::ProviderReady { url, discovery } => self.provider_ready(cx, &url, discovery),
            // From a server the client has since left (a provider switch after
            // startup's first look), or overtaken by a switch under way.
            UiEvent::ServerDiscovered(disc) if self.provider_switch.is_some() || disc.base_url != cx.source.0.base_url() => {}
            UiEvent::ServerDiscovered(disc) => {
                self.server_silent = false;
                self.on_server_discovered(cx, disc)
            }
            UiEvent::ServerSilent(url) => self.server_silent = self.provider_switch.is_none() && url == cx.source.0.base_url(),
            UiEvent::Loop { turn_id, event } => self.on_loop_event(cx, turn_id, &event),
            UiEvent::Finished { turn_id, result } => return self.finish_turn(cx, turn_id, result).await,
            UiEvent::Resize(cols, rows) => {
                let term_resized = (cols, rows) != self.last_term_size;
                self.last_term_size = (cols, rows);
                if !self.chat.has_user_message() && term_resized {
                    self.animate_welcome(cx.source, cx.mascot_mood, Some(cols as usize), None);
                }
                self.renderer.request_reprint();
            }
            UiEvent::Mouse(m) => self.on_mouse(m),
            UiEvent::Paste(pasted) => {
                self.postpone_recap();
                self.on_paste(cx, &pasted);
            }
            UiEvent::TaskEnded(notice) => self.on_task_ended(cx, notice),
            UiEvent::Key(code, mods) => {
                self.postpone_recap();
                let flow = match self.handle_overlay_key(cx, code, mods).await {
                    Flow::Next => self.handle_key(cx, code, mods).await,
                    claimed => claimed,
                };
                if matches!(flow, Flow::Quit) && !self.quit_confirmed(cx) {
                    return Flow::Continue;
                }
                return flow;
            }
        }
        Flow::Next
    }

    /// The face shows whether the model server answered. Discovery reruns,
    /// so starting the server later turns it around on its own. Offline once
    /// a look got no answer, not merely because it is slow: a cloud API's
    /// model list can take longer than a few seconds, and was reported down.
    pub(crate) fn server_mood(&self, source: &BackendSource, started_at: std::time::Instant) -> MascotMood {
        server_mood(self.provider_switch.is_some(), source.discovery().is_some(), self.server_silent, started_at.elapsed())
    }

    /// An unreachable server is announced once, and taken back when it answers.
    pub(crate) fn announce_mood(&mut self, mood: MascotMood, source: &BackendSource) {
        if mood == self.announced_mood {
            return;
        }
        let previous = std::mem::replace(&mut self.announced_mood, mood);
        match mood {
            MascotMood::Offline => {
                let url = source.0.base_url();
                // "Start it" is for a server on this machine, not for a cloud API.
                let local = flashagent_core::url_host(&url).is_some_and(|host| flashagent_core::is_local_host(&host));
                let next = if local { "start the server" } else { "check the address and the connection" };
                self.background = Some(BackgroundNotice::sticky(format!("{OFFLINE_NOTICE} {url} \u{b7} {next}, or /provider switches to another")).warning());
            }
            MascotMood::Happy if previous == MascotMood::Offline => {
                self.background = Some(BackgroundNotice::fading(
                    format!("Model server is answering \u{b7} {}", self.current_model),
                    5,
                ));
            }
            _ => {}
        }
    }

    /// Only while the prompt is a command and nothing else has the keyboard.
    pub(crate) fn autocomplete_popup(&self, gate: &TuiGate, question_gate: &TuiQuestionGate) -> Option<AutocompletePopup> {
        if self.running || !self.input.starts_with('/') || self.overlay.is_some() || gate.pending().is_some() || question_gate.pending().is_some() {
            return None;
        }
        AutocompletePopup::for_input(&self.input, std::path::Path::new("."), self.autocomplete_idx)
    }

    pub(crate) fn on_channel_target(&mut self, target: ChannelTarget) {
        if let Some(sw) = self.channel_switch.as_mut() {
            sw.target = target;
            self.renderer.request_reprint();
        }
    }

    pub(crate) fn on_update_notice(&mut self, notice: UpdateNotice) {
        match notice {
            UpdateNotice::Available { version, asset_name, download_url, checksums_url } => {
                self.pending_update = Some((version.clone(), asset_name, download_url, checksums_url));
                self.background = Some(BackgroundNotice::sticky(format!(
                    "Update available: {version} · /update installs it"
                )));
                if let Some(s) = self.settings_view_mut() {
                    s.update_check_status = Some(format!("Available: {version} (/update)"));
                }
            }
            UpdateNotice::Progress { version, stage } => {
                // Kept either way, so /update can show a download that started unasked.
                if self.update_watched {
                    BackgroundNotice::update_sticky(&mut self.background, update_progress_line(&version, stage));
                }
                self.update_progress = Some((version, stage));
            }
            UpdateNotice::Ready { version } => {
                self.pending_update = None;
                self.update_progress = None;
                self.update_watched = false;
                self.background = Some(BackgroundNotice::sticky(format!(
                    "Updated to {version} \u{b7} restart FlashAgent to use it"
                )));
                if let Some(s) = self.settings_view_mut() {
                    s.update_check_status = Some(format!("Ready: {version} (restart to apply)"));
                }
            }
            UpdateNotice::UpToDate { version } => {
                if let Some(s) = self.settings_view_mut() {
                    s.update_check_status = Some(format!("Up to date ({version})"));
                }
                if std::mem::take(&mut self.update_watched) {
                    self.background = Some(BackgroundNotice::fading(
                        format!("FlashAgent {version} is up to date"),
                        6,
                    ));
                }
            }
            UpdateNotice::Failed { error } => {
                self.update_progress = None;
                if let Some(s) = self.settings_view_mut() {
                    s.update_check_status = Some(format!("Error: {error}"));
                }
                if std::mem::take(&mut self.update_watched) {
                    self.background = Some(
                        BackgroundNotice::fading(
                            format!("Update failed: {}", flashagent_tui::truncate_middle(&error, 90)),
                            10,
                        )
                        .warning(),
                    );
                }
            }
        }
        self.renderer.request_reprint();
    }

    /// The idle clock: tips, the recap, expiring notices, a turn that would not
    /// stop, and the welcome card while the conversation has not started.
    pub(crate) fn on_tick(&mut self, cx: &LoopCtx<'_>) {
        self.tick_n += 1;
        self.tip_animator.tick();
        self.start_recap_if_due(cx.source, cx.tx);
        // A question that timed out (a /goal one after 120 s) goes without a
        // key; its selection and half-written answer must not open the next.
        if cx.question_gate.pending().is_none() {
            self.question_ui_state = QuestionUiState::default();
        }
        if self.background.as_ref().is_some_and(BackgroundNotice::expired) {
            self.background = None;
            self.renderer.request_reprint();
        }
        if self.running && self.cancel_requested.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(3)) {
            self.abort_stuck_turn(cx);
        }
        let current_term_size = crossterm::terminal::size().unwrap_or((100, 24));
        let term_resized = current_term_size != self.last_term_size;
        if term_resized {
            self.last_term_size = current_term_size;
        }
        // Rebuilt only on ticks where the card actually looks different.
        let reveal_rows = welcome_reveal_rows(cx.started_at);
        let mood_changed = cx.mascot_mood != self.last_mascot_mood;
        self.last_mascot_mood = cx.mascot_mood;
        if !self.running
            && !self.chat.has_user_message()
            && (term_resized
                || mood_changed
                || reveal_rows.is_some()
                || flashagent_tui::mascot_needs_repaint(self.tick_n))
        {
            self.animate_welcome(cx.source, cx.mascot_mood, Some(current_term_size.0 as usize), reveal_rows);
        }
        if term_resized {
            self.renderer.request_reprint();
        }
    }

    /// The loop did not wind down in time (a tool ignoring cancellation). History
    /// keeps the prompt but not the partial turn, and the user is told.
    fn abort_stuck_turn(&mut self, cx: &LoopCtx<'_>) {
        if let Some(handle) = self.active_turn_handle.take() {
            handle.abort();
        }
        self.running = false;
        self.turn_started = None;
        self.cancel_requested = None;
        self.aborted_turn = Some(self.turn_counter);
        close_dangling_user(&mut self.history, "[turn aborted by the user]");
        self.task_inbox.turn_aborted();
        self.flush_task_lines();
        self.token_tracker.on_finished();
        if let Some(saved) = self.goal_state.take() {
            cx.tools_arc.set_goal_mode(false);
            cx.perm.state().set_goal_active(false);
            cx.perm.state().set_mode(saved.mode);
            self.current_effort = saved.effort.clone();
            self.max_steps = saved.max_steps;
            if let Some(ledger) = self.goal_ledger.take() {
                push_goal_report(&mut self.chat, &ledger, DoneReason::Cancelled);
            }
        }
        self.chat.on_event(&LoopEvent::Done(DoneReason::Cancelled));
        self.flash_turn_end(Some(DoneReason::Cancelled));
        self.custom_placeholder = Some("Turn aborted; its partial output was not kept in the model context".to_string());
        self.renderer.request_reprint();
    }

    /// Asks the server again what it runs, unless a turn or another look is
    /// already talking to it.
    pub(crate) fn poll_server(&mut self, cx: &LoopCtx<'_>) {
        self.warm_prompt_cache(cx.source, cx.perm, cx.memory_block);
        if !should_poll_server(self.running, cx.source.0.requests_in_flight(), cx.is_discovering.load(Ordering::Relaxed)) {
            return;
        }
        cx.is_discovering.store(true, Ordering::Relaxed);
        let source = cx.source.clone();
        let tx = cx.tx.clone();
        let flag = cx.is_discovering.clone();
        tokio::spawn(async move {
            let url = source.0.base_url();
            let _ = tx.send(match source.discover_server().await {
                Some(disc) => UiEvent::ServerDiscovered(disc),
                None => UiEvent::ServerSilent(url),
            });
            flag.store(false, Ordering::Relaxed);
        });
    }

    pub(crate) fn on_server_discovered(&mut self, cx: &LoopCtx<'_>, disc: flashagent_llm::ServerDiscovery) {
        self.available_models = flashagent_tui::providers::offered_models(&disc);
        // A cloud listing names no loaded model, so the one above never
        // renames `models/gemini-…`, saved that way, when the listing is late.
        if let Some(listed) = disc.active_model.is_none().then(|| flashagent_tui::providers::listed_name(&self.current_model, &disc)).flatten() {
            self.current_model = listed;
            cx.source.set_model(&self.current_model);
            if self.on_active_provider(cx.source) {
                self.config.active_profile_mut().model = self.current_model.clone();
                self.save_config();
            }
            self.refresh_welcome(cx.source, cx.mascot_mood);
            self.renderer.request_reprint();
        }
        // Kept rather than swapped for a model that would be billed instead,
        // so the user hears about it before the first request fails.
        if disc.active_model.is_none() && self.on_active_provider(cx.source) && self.config.endpoint().api_key.is_some() && !self.current_model.is_empty() && disc.model(&self.current_model).is_none() {
            self.custom_placeholder = Some(format!("{} does not list {} · F3 picks one of its models", self.config.active_profile().name, self.current_model));
            self.suggested_prompt = None;
            self.renderer.request_reprint();
        }
        if let Some(active) = disc.active_model {
            let new_ctx_len = active.context_length.or(active.max_context_length).unwrap_or(131_072);
            let new_ctx_disp = active.context_display();
            let model_changed = active.id != self.current_model;
            let ctx_changed = self.context_usage.total_capacity != new_ctx_len || self.current_context != new_ctx_disp;

            if model_changed || ctx_changed {
                let old_m = self.current_model.clone();
                let old_ctx_len = self.context_usage.total_capacity;
                self.current_model = active.id.clone();
                self.current_context = new_ctx_disp;
                self.context_usage.total_capacity = new_ctx_len.max(1024);
                cx.tools_arc.set_context_window(Some(new_ctx_len));
                cx.source.set_model(&self.current_model);
                cx.source.set_effort_bias(self.effort_memory.steps(&self.current_model));
                cx.tools_arc.set_vision_supported(model_sees_images(cx.source, &self.current_model));
                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);

                // Only for the provider the client talks to: another's model
                // must not be written beside it.
                if model_changed && self.on_active_provider(cx.source) {
                    self.config.active_profile_mut().model = self.current_model.clone();
                    self.save_config();
                }
                if model_changed {
                    // The effort is the user's choice. A non-reasoning model just gets no
                    // thinking fields; turning "auto" into "off" here lost it for later models.
                    if self.current_effort.is_empty() {
                        self.current_effort = "auto".to_string();
                    }
                }

                self.refresh_welcome(cx.source, cx.mascot_mood);
                let ctx_tag = self.current_context.clone().unwrap_or_default();

                if model_changed {
                    let msg = format!(
                        "Server active model switched: {old_m} -> {}", self.current_model
                    );
                    self.custom_placeholder = Some(msg);
                    self.suggested_prompt = None;
                } else if ctx_changed {
                    let old_formatted = ContextUsage::format_tokens(old_ctx_len);
                    let new_formatted = ContextUsage::format_tokens(new_ctx_len);
                    let msg = format!(
                        "Model context capacity: {old_formatted} -> {new_formatted} ({ctx_tag})"
                    );
                    self.custom_placeholder = Some(msg);
                    self.suggested_prompt = None;
                }
                self.renderer.request_reprint();
            }
        }
        self.warm_prompt_cache(cx.source, cx.perm, cx.memory_block);
    }

    fn on_recap(&mut self, turn_id: u64, recap: &str, suggestion: Option<String>) {
        let formatted = format!("  \x1b[38;2;155;165;180mrecap:\x1b[0m \x1b[38;2;225;230;240m{recap}\x1b[0m");
        // The ordinal of the user message the recap is about; regenerate and steering
        // make turn_counter drift.
        let current_turn = self.chat.user_turn_count() as u64;
        if turn_id == current_turn {
            self.chat.update_or_push_turn_system("recap:", &formatted);
            self.latest_suggestion = suggestion.clone();
            self.custom_placeholder = None;
            if self.input.is_empty() && self.active_turn_handle.is_none() {
                self.suggested_prompt = suggestion;
            }
            self.renderer.request_reprint();
        } else if turn_id < current_turn {
            self.chat.attach_turn_recap(turn_id, &formatted);
            self.renderer.request_reprint();
        }
    }

    fn on_memory_summary(&mut self, result: Result<flashagent_tui::memory_view::MemorySummary, String>) {
        if let Ok(summary) = &result {
            save_summary(summary);
        }
        if let Some(Overlay::Memory(modal)) = self.overlay.as_mut() {
            modal.summary = match result {
                Ok(summary) => flashagent_tui::memory_view::SummaryState::Ready(summary),
                Err(why) => flashagent_tui::memory_view::SummaryState::Failed(why),
            };
        }
        self.renderer.request_reprint();
    }

    fn on_loop_event(&mut self, cx: &LoopCtx<'_>, turn_id: u64, e: &LoopEvent) {
        // Late events of an aborted or superseded turn.
        if turn_id != self.turn_counter || self.aborted_turn == Some(turn_id) {
            return;
        }
        self.track_turn(e);
        self.track_goal(cx.cwd, e);
        self.chat.on_event(e);
        if self.chat.take_needs_reprint() {
            self.renderer.request_reprint();
        }
        update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
    }

    /// The live phase, the token rate and what the turn did, for the status
    /// line and the effort memory.
    fn track_turn(&mut self, e: &LoopEvent) {
        match e {
            LoopEvent::TurnDelta(text) => {
                self.token_tracker.on_delta(text);
                self.turn_outcome.answer_chars += text.chars().count();
                self.turn_phase = TurnPhase::Writing;
            }
            LoopEvent::ReasoningDelta(text) => {
                self.token_tracker.on_delta(text);
                self.turn_outcome.reasoning_chars += text.chars().count();
                self.turn_phase = TurnPhase::Thinking;
            }
            LoopEvent::ToolStarted { name, args_json, .. } => {
                self.last_tool_name = Some(name.clone());
                self.turn_outcome.tool_calls += 1;
                self.token_tracker.on_delta(name);
                self.token_tracker.on_delta(args_json);
                self.turn_phase = TurnPhase::Tool;
            }
            LoopEvent::ToolFinished { is_error, result, .. } => {
                if *is_error {
                    self.turn_outcome.failed_tools += 1;
                }
                // Memory is written unasked, so it is announced, on the line under the input.
                let wrote_memory = self.last_tool_name
                    .as_deref()
                    .is_some_and(|n| matches!(n, "memory_create" | "memory_update" | "memory_remove"));
                if wrote_memory && !*is_error {
                    if let Some(said) = result.as_deref().and_then(|r| r.lines().next()) {
                        self.background = Some(BackgroundNotice::fading(
                            format!("{said}  ·  /memory to see or change it"),
                            10,
                        ));
                    }
                }
                self.turn_phase = TurnPhase::AfterTool;
            }
            LoopEvent::StepStarted { step, .. } if *step > 1 => {
                self.turn_phase = TurnPhase::AfterTool;
            }
            LoopEvent::Usage(u) => {
                self.token_tracker.on_usage(u);
            }
            LoopEvent::SteeringInjected(directive) if self.task_inbox.injected(directive) => {
                self.flush_task_lines();
                self.renderer.request_reprint();
            }
            LoopEvent::SteeringInjected(directive) => {
                if let Some(pos) = self.pending_steers.iter().position(|s| s == directive) {
                    self.pending_steers.remove(pos);
                } else if !self.pending_steers.is_empty() {
                    self.pending_steers.remove(0);
                }
                self.renderer.request_reprint();
            }
            _ => {}
        }
    }

    /// A /goal's plan and its milestone commits.
    fn track_goal(&mut self, cwd: &std::path::Path, e: &LoopEvent) {
        let Some(ledger) = self.goal_ledger.as_mut() else { return };
        let plan_changed = ledger.on_event(e);
        if plan_changed {
            let block = format_plan(ledger.plan());
            self.chat.update_or_push_turn_system("plan:", &block);
            self.renderer.request_reprint();
        }
        if let LoopEvent::StepStarted { step, .. } = e {
            self.renderer.request_reprint();
            let completed = step.saturating_sub(1);
            if completed > 0 && completed % MILESTONE_COMMIT_INTERVAL == 0 {
                if let Some(note) = commit_goal_milestone(ledger, cwd, completed) {
                    self.chat.push_system(&note);
                    self.renderer.request_reprint();
                }
            }
        }
    }

    fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        if let Some(menu) = self.overlay.as_mut().and_then(Overlay::select_menu_mut) {
            match m.kind {
                MouseEventKind::ScrollUp => menu.up(),
                MouseEventKind::ScrollDown => menu.down(),
                _ => {}
            }
            self.renderer.request_reprint();
            return;
        }
        // A screen that is not a list: the wheel must not scroll the transcript behind it.
        if self.overlay.is_some() {
            return;
        }
        match m.kind {
            MouseEventKind::ScrollUp => self.renderer.scroll_up(3),
            MouseEventKind::ScrollDown => self.renderer.scroll_down(3),
            // A click on a thought or a tool call opens or folds that one alone.
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                let expansion = ReasoningExpansion { all: self.all_expanded, last: self.last_expanded };
                if let Some(row) = self.renderer.chat_row_at(m.row) {
                    self.chat.toggle_row(row, expansion);
                }
            }
            _ => {}
        }
    }

    /// A paste goes to whatever has the keyboard first: a path pasted as a
    /// question's answer must not become an attachment.
    fn on_paste(&mut self, cx: &LoopCtx<'_>, pasted: &str) {
        let one_line = || pasted.replace("\r\n", " ").replace(['\n', '\r'], " ");
        if let Some(req) = cx.question_gate.pending() {
            // As typing does: the card turns to the answer of one's own.
            let state = &mut self.question_ui_state;
            if let Some(opts) = req.options.as_ref().filter(|_| !state.is_writing) {
                state.selected_index = opts.len();
                state.is_writing = true;
                state.write_in_text.clear();
            }
            state.write_in_text.insert_str(&one_line());
        } else if let Some(Overlay::Sampling(sm)) = self.overlay.as_mut() {
            for ch in one_line().chars() {
                sm.handle_key(KeyCode::Char(ch), KeyModifiers::NONE);
            }
        } else if let Some(Overlay::Providers(view)) = self.overlay.as_mut() {
            view.handle_paste(pasted);
        } else if let Some(menu) = self.overlay.as_mut().and_then(Overlay::select_menu_mut) {
            // A menu's filter (F3, Ctrl+K) takes the paste as typing.
            for ch in one_line().chars() {
                menu.push_filter_char(ch);
            }
        } else if self.history_search.is_some() {
            // Ctrl+F: into the search, not the hidden prompt under it.
            for ch in one_line().chars() {
                self.history_search_key(KeyCode::Char(ch), KeyModifiers::NONE);
            }
        } else if self.overlay.is_some() || cx.gate.pending().is_some() {
            // Nothing there takes text.
        } else if let Some(att) = Attachment::from_dropped_path(pasted) {
            // A dropped picture's path means the picture.
            let label = att.label();
            self.attachments.push(att);
            self.suggested_prompt = None;
            self.background = Some(BackgroundNotice::fading(format!("{label} attached · Ctrl+Z removes it"), 8));
        } else if !pasted.is_empty() {
            // Pasted code or logs keep their line breaks.
            self.input.insert_str(pasted);
            self.history_index = None;
            self.autocomplete_idx = 0;
        }
        self.renderer.request_reprint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_server_is_not_called_down_until_a_look_gets_no_answer() {
        use std::time::Duration;
        let secs = Duration::from_secs;
        assert_eq!(server_mood(false, false, false, secs(8)), MascotMood::Checking, "a slow cloud list is still coming");
        assert_eq!(server_mood(false, false, true, secs(8)), MascotMood::Offline);
        assert_eq!(server_mood(false, false, true, secs(1)), MascotMood::Checking, "no flash before the first frame settles");
        assert_eq!(server_mood(false, false, false, secs(31)), MascotMood::Offline, "nothing for half a minute");
        assert_eq!(server_mood(false, true, true, secs(40)), MascotMood::Happy);
        assert_eq!(server_mood(true, false, true, secs(40)), MascotMood::Checking);
    }
}
