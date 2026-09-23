use super::*;

impl App {
    /// A new turn is starting: on a local server the recap would make it wait, and
    /// its suggestion would be stale. Dropping the request stops the generation.
    pub(crate) fn cancel_recap(&mut self) {
        self.recap_due = None;
        if let Some(task) = self.recap_task.take() {
            task.abort();
        }
    }

    /// The recap is one more request to the model, which on a local server holds
    /// the next question up. It is written only once the user has gone quiet.
    pub(crate) fn schedule_recap(&mut self) {
        self.cancel_recap();
        self.recap_due = Some(std::time::Instant::now() + recap_idle());
    }

    /// Typing means the user is still here.
    pub(crate) fn postpone_recap(&mut self) {
        if let Some(due) = self.recap_due.as_mut() {
            *due = (*due).max(std::time::Instant::now() + recap_idle());
        }
    }

    /// Once due, with no turn running.
    pub(crate) fn start_recap_if_due(&mut self, source: &Arc<BackendSource>, tx: &tokio::sync::mpsc::UnboundedSender<UiEvent>) {
        if self.running || !self.recap_due.is_some_and(|due| std::time::Instant::now() >= due) {
            return;
        }
        self.recap_due = None;
        let source_bg = source.clone();
        let tx_bg = tx.clone();
        let history_bg = self.history.clone();
        let turn_id = self.chat.user_turn_count() as u64;
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

    /// Cooperative: the loop answers pending tool calls and returns its history,
    /// so the model's memory matches the screen. The tick loop aborts it if it
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

    /// Keeps a compacted summary at the end of the system prompt.
    pub(crate) fn apply_personality(&mut self) {
        let base = build_system_prompt(&self.prompt_config.clone().with_personality(&self.config.personality));
        if let Some(first) = self.history.first_mut().filter(|m| m.role == flashagent_llm::Role::System) {
            let summary = first.content.find(COMPACTED_MARK).map(|i| first.content[i..].to_string()).unwrap_or_default();
            first.content = base + &summary;
        }
    }

    /// The last message in the history is what the model answers.
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
            self.config.personality.voice_prelude(),
        ));
    }

    /// Ctrl+R and /regenerate. False when there is no prompt to answer again.
    pub(crate) fn regenerate(&mut self, cx: &LoopCtx<'_>) -> bool {
        let Some(user_idx) = self.history.iter().rposition(|m| m.role == flashagent_llm::Role::User) else {
            return false;
        };
        // Asking again says the last answer was not good enough: the one signal the
        // model had too little room to think.
        let steps = self.effort_memory.observe(
            &self.current_model,
            &flashagent_core::TurnOutcome { regenerated: true, ..Default::default() },
        );
        self.effort_memory.save();
        cx.source.set_effort_bias(steps);
        self.history.truncate(user_idx + 1);
        // Same message, same turn, so rewind reaches before the first attempt, even
        // after --resume.
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
        self.last_expanded = false;
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
        true
    }

    pub(crate) async fn finish_turn(
        &mut self,
        cx: &mut LoopCtx<'_>,
        turn_id: u64,
        res: Result<(Vec<ChatMessage>, DoneReason), (String, Vec<ChatMessage>)>,
    ) -> Flow {
        if turn_id != self.turn_counter || self.aborted_turn == Some(turn_id) {
            // A hard-aborted or superseded turn reporting late.
            return Flow::Continue;
        }
        self.running = false;
        self.turn_started = None;
        self.active_turn_handle = None;
        self.active_steer_tx = None;
        // A steer typed as the turn ended never reached the model; it goes back into
        // the prompt.
        let unsent = std::mem::take(&mut self.pending_steers);
        if !unsent.is_empty() && self.input.is_empty() {
            self.input.set(unsent.join(" "));
            self.background = Some(BackgroundNotice::fading(
                "The turn ended before your message reached it · Enter sends it now",
                8,
            ));
        }
        self.cancel_requested = None;
        cx.cancel.store(false, Ordering::Relaxed);
        self.token_tracker.on_finished();
        self.flash_turn_end(res.as_ref().ok().map(|(_, reason)| *reason));

        // Cost against output is the evidence for whether auto guessed right. Goal
        // runs are excluded: their effort is the user's. This turn's growth predicts
        // the next one's.
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
                // The turn just read all of it. A later change (interrupted turn closed,
                // compaction) makes a new prefix, which is warmed.
                self.note_prompt_cached(cx.perm, cx.memory_block);
                self.renderer.request_reprint();

                if reason == DoneReason::Cancelled {
                    close_dangling_user(&mut self.history, "[interrupted by the user before replying]");
                    self.suggested_prompt = None;
                    let interrupt_msg = "Request interrupted by user";
                    self.custom_placeholder = Some(interrupt_msg.to_string());
                    self.renderer.request_reprint();
                } else {
                    // Only when the next turn would not fit or the threshold is passed, and there
                    // is something worth summarising.
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
                        // In the transcript: it changes what the model remembers.
                        self.chat.push_system("Compacting context...");
                        self.renderer.request_reprint();
                        // Drawn before the wait: the command is already gone from the composer.
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

                    self.autosave(cx.session_id, cx.cwd_display);

                    // Recap and suggestions come only from the background model call, never
                    // from fallback strings.
                    self.suggested_prompt = None;
                    self.renderer.request_reprint();

                    self.schedule_recap();
                }
            }
            Err((e, h)) => {
                // Keep the steps that already ran and changed files.
                self.history = h;
                close_dangling_user(&mut self.history, "[no reply: the model backend failed]");
                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                self.chat.on_event(&LoopEvent::Done(DoneReason::Failed));
                // Plain explanation first, the raw text underneath.
                let explained = flashagent_tui::backend_error::explain(
                    &e,
                    &self.config.backend_url,
                    &self.current_model,
                );
                self.chat.push_line(LineKind::ToolError, explained.headline.clone());
                if let Some(hint) = &explained.hint {
                    // Its own line: the renderer clips embedded newlines instead of wrapping.
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

/// Three minutes without a key or a turn. `FLASHAGENT_RECAP_IDLE_SECS` sets it
/// (the scenario tests use 0).
fn recap_idle() -> std::time::Duration {
    static IDLE: std::sync::OnceLock<std::time::Duration> = std::sync::OnceLock::new();
    *IDLE.get_or_init(|| {
        let secs = std::env::var("FLASHAGENT_RECAP_IDLE_SECS").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(180);
        std::time::Duration::from_secs(secs)
    })
}
