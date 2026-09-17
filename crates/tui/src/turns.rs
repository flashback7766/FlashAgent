use super::*;

impl App {
    /// Stop a recap and suggestion still being written for the last turn.
    /// A new turn is about to start: on a local server the recap request
    /// would make it wait its turn, and its suggestion would be about a
    /// conversation that has already moved on. Dropping the request closes
    /// the connection, which stops the server generating it.
    pub(crate) fn cancel_recap(&mut self) {
        if let Some(task) = self.recap_task.take() {
            task.abort();
        }
    }

    /// Esc or Ctrl+C during a turn. Cooperative: the loop answers pending
    /// tool calls, keeps partial text and returns its history via Finished,
    /// so model memory matches the screen; the tick loop aborts it if it
    /// does not wind down in time.
    pub(crate) fn interrupt(&mut self, cx: &LoopCtx<'_>) {
        if self.running && self.cancel_requested.is_none() {
            cx.cancel.store(true, Ordering::Relaxed);
            self.cancel_requested = Some(std::time::Instant::now());
            self.turn_phase = TurnPhase::Stopping;
            self.active_steer_tx = None;
            self.pending_steers.clear();
            self.notice("Interrupting...");
        }
        self.renderer.request_reprint();
    }

    /// Rebuild the system prompt for the current style and tone, keeping a
    /// compacted summary that rides at its end.
    pub(crate) fn apply_personality(&mut self) {
        let base = build_system_prompt(&self.prompt_config.clone().with_personality(&self.config.personality));
        if let Some(first) = self.history.first_mut().filter(|m| m.role == flashagent_llm::Role::System) {
            let summary = first.content.find(COMPACTED_MARK).map(|i| first.content[i..].to_string()).unwrap_or_default();
            first.content = base + &summary;
        }
    }

    /// Start a turn on the history as it stands: the last message in it is
    /// what the model answers.
    pub(crate) fn start_turn(&mut self, cx: &LoopCtx<'_>, budgets: GoalBudgets) {
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
        self.speed_history.clear();
        self.turn_flash = None;
        let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
        self.active_steer_tx = Some(steer_tx);
        self.pending_steers.clear();
        self.cancel_recap();
        self.active_turn_handle = Some(spawn_turn(
            cx.cancel.clone(),
            cx.source.clone(),
            cx.perm,
            self.history.clone(),
            budgets,
            turn_opts,
            cx.tx.clone(),
            steer_rx,
            self.turn_counter,
        ));
    }

    /// Ctrl+R and /regenerate: drop the last answer and ask again. Returns
    /// false when there is no prompt to answer again.
    pub(crate) fn regenerate(&mut self, cx: &LoopCtx<'_>) -> bool {
        let Some(user_idx) = self.history.iter().rposition(|m| m.role == flashagent_llm::Role::User) else {
            return false;
        };
        // Asking for the same answer again is the user saying the last one
        // was not good enough — the one signal that the model was given too
        // little room to think.
        let steps = self.effort_memory.observe(
            &self.current_model,
            &flashagent_core::TurnOutcome { regenerated: true, ..Default::default() },
        );
        self.effort_memory.save();
        cx.source.set_effort_bias(steps);
        self.history.truncate(user_idx + 1);
        // Same message, same turn: taking it back must reach before the first
        // attempt, even after --resume.
        if let Some(store) = cx.perm.state().snapshots() {
            let prompt_for_snapshot = if self.history[user_idx].content.is_empty() {
                "[image]"
            } else {
                &self.history[user_idx].content
            };
            store.continue_turn(prompt_for_snapshot);
        }
        self.chat.truncate_to_last_user();
        self.renderer.scroll_to_bottom();
        self.renderer.printed_settled = 0;
        self.renderer.prev_expansion = None;
        self.renderer.request_reprint();
        self.last_expanded = false;
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
        true
    }

    /// A turn ended: apply its history, report how it went, compact the
    /// context if it needs it, and ask for a recap.
    pub(crate) async fn finish_turn(
        &mut self,
        cx: &mut LoopCtx<'_>,
        turn_id: u64,
        res: Result<(Vec<ChatMessage>, DoneReason), (String, Vec<ChatMessage>)>,
    ) -> Flow {
        if turn_id != self.turn_counter || self.aborted_turn == Some(turn_id) {
            // A turn that was hard-aborted (or superseded) reporting late.
            return Flow::Continue;
        }
        self.running = false;
        self.turn_started = None;
        self.active_turn_handle = None;
        self.active_steer_tx = None;
        // A steer typed as the turn was ending never reached the model; it
        // goes back in the prompt rather than vanishing.
        let unsent = std::mem::take(&mut self.pending_steers);
        if !unsent.is_empty() && self.input.is_empty() {
            self.input = unsent.join(" ");
            self.background = Some(BackgroundNotice::fading(
                "The turn ended before your message reached it · Enter sends it now",
                8,
            ));
        }
        self.cancel_requested = None;
        cx.cancel.store(false, Ordering::Relaxed);
        self.token_tracker.on_finished();
        self.flash_turn_end(res.as_ref().ok().map(|(_, reason)| *reason));

        // What the turn cost against what it produced is the only
        // honest evidence about whether auto guessed right for this
        // model. A goal run is excluded: its effort is the user's.
        // What this turn cost the window is the best guess at what
        // the next one will cost.
        let grown = self.context_usage.total_used().saturating_sub(self.context_before_turn);
        if grown > 0 {
            self.last_turn_growth = grown.max(self.last_turn_growth / 2);
        }
        if self.goal_state.is_none() {
            let steps = self.effort_memory.observe(&self.current_model, &self.turn_outcome);
            self.effort_memory.save();
            cx.source.set_effort_bias(steps);
        }
        self.turn_outcome = flashagent_core::TurnOutcome::default();

        // Roll back any temporary goal state
        if let Some(saved) = self.goal_state.take() {
            cx.tools_arc.set_goal_mode(false);
            cx.perm.state().set_goal_active(false);
            cx.perm.state().set_mode(saved.mode);
            self.current_effort = saved.effort.clone();
            self.max_steps = saved.max_steps;
            if let Some(ledger) = self.goal_ledger.take() {
                let reason = match &res {
                    Ok((_, r)) => *r,
                    Err(_) => DoneReason::Failed,
                };
                push_goal_report(&mut self.chat, &ledger, reason);
            }
            self.notice(format!(
                "[Goal \"{}\" finished · Restored mode to {} and thinking to {}]",
                flashagent_tui::truncate_middle(&saved.task, 40),
                saved.mode.label(),
                saved.effort
            ));
        }

        match res {
            Ok((h, reason)) => {
                self.history = h;
                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                // Completed turn telemetry and cache hit are displayed on Line 3 of the footer.
                self.renderer.request_reprint();

                if reason == DoneReason::Cancelled {
                    close_dangling_user(&mut self.history, "[interrupted by the user before replying]");
                    self.suggested_prompt = None;
                    let interrupt_msg = "Request interrupted by user";
                    self.custom_placeholder = Some(interrupt_msg.to_string());
                    self.renderer.request_reprint();
                } else {
                    // Summarise the older part of the conversation
                    // when the next turn would not fit, or the user's
                    // threshold is passed — and only when there is
                    // something worth summarising.
                    let verdict = if self.config.auto_compact_context && self.history.len() > 3 {
                        flashagent_core::should_compact(flashagent_core::CompactionInput {
                            used: self.context_usage.total_used(),
                            capacity: self.context_usage.total_capacity,
                            fixed: self.context_usage.system_tokens
                                + self.context_usage.memory_tokens
                                + self.context_usage.tools_tokens,
                            last_turn_growth: self.last_turn_growth,
                            threshold_pct: flashagent_core::resolved_compact_threshold(
                                self.config.context_compact_threshold,
                                self.context_usage.total_capacity,
                            ),
                        })
                    } else {
                        flashagent_core::CompactionVerdict::No
                    };
                    if verdict.should() {
                        // This one belongs in the transcript: it
                        // changes what the model remembers, which is
                        // the conversation itself.
                        self.chat.push_system("Compacting context...");
                        self.renderer.request_reprint();
                        // Shown before the wait: the command that started it
                        // is gone from the composer, and its suggestions with it.
                        self.draw(cx, None);
                        let source_compact = cx.source.clone();
                        let before = self.context_usage.total_used();
                        match compact_context(&source_compact, &mut self.history, None).await {
                            Some(_) => {
                                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                                let saved = before.saturating_sub(self.context_usage.total_used());
                                self.chat.replace_last_system(&format!(
                                    "Context compacted · {} saved · the conversation so far is now a summary",
                                    ContextUsage::format_tokens(saved)
                                ));
                            }
                            None => self.chat.replace_last_system(
                                "Compacting context failed — the conversation is unchanged",
                            ),
                        }
                        self.renderer.request_reprint();
                    }

                    if self.config.auto_save_sessions {
                        save_session_file(cx.session_id, &self.current_model, cx.cwd_display, &self.history);
                    }

                    // Clear any existing ghost suggestion.
                    // No hardcoded or heuristic fallback strings for recap or write-in suggestions:
                    // they are ONLY shown if and when dynamically generated by the background LLM task.
                    self.suggested_prompt = None;
                    self.renderer.request_reprint();

                    // Asynchronously ask LLM in background for refined recap & contextual suggestion
                    let source_bg = cx.source.clone();
                    let tx_bg = cx.tx.clone();
                    let history_bg = self.history.clone();
                    let turn_id = self.chat.user_turn_count() as u64;
                    self.cancel_recap();
                    self.recap_task = Some(tokio::spawn(async move {
                        if let Some((llm_recap, llm_suggestion)) = generate_llm_recap_and_suggestion(&source_bg, &history_bg).await {
                            let _ = tx_bg.send(UiEvent::BackgroundRecap {
                                turn_id,
                                recap: llm_recap,
                                suggestion: llm_suggestion,
                            });
                        }
                    }));
                }
            }
            Err((e, h)) => {
                // Keep the steps that already ran (and changed files).
                self.history = h;
                close_dangling_user(&mut self.history, "[no reply: the model backend failed]");
                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                self.chat.on_event(&LoopEvent::Done(DoneReason::Failed));
                // What broke and what to do about it, with the raw
                // text kept underneath rather than as the headline.
                let explained = flashagent_tui::backend_error::explain(
                    &e,
                    &self.config.backend_url,
                    &self.current_model,
                );
                self.chat.push_line(LineKind::ToolError, explained.headline.clone());
                if let Some(hint) = &explained.hint {
                    // Its own line: an embedded newline is not wrapped
                    // by the renderer, it is clipped.
                    self.chat.push_line(
                        LineKind::System,
                        format!("  \x1b[38;2;160;155;145m{hint}\x1b[0m"),
                    );
                }
                if explained.headline != explained.raw.trim() {
                    self.chat.push_line(
                        LineKind::System,
                        format!("  \x1b[38;2;120;115;110m{}\x1b[0m", explained.raw.trim()),
                    );
                }
                self.notice("Ctrl+R retries the last prompt");
            }
        }

        Flow::Next
    }
}
