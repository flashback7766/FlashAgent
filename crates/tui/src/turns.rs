use super::*;

/// How long the run took, and no clock left behind. One operation, so the time
/// cannot be read after the thing that holds it has been cleared.
pub(crate) fn elapsed(started: &mut Option<std::time::Instant>) -> Option<f32> {
    let took = started.take()?.elapsed().as_secs_f32();
    Some(took)
}

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

    /// What the app knows about work in flight, gathered for [`recap_blocked_by`].
    pub(crate) fn recap_blocked_by(&self, cx: &LoopCtx<'_>) -> Option<&'static str> {
        recap_blocked_by(RecapBusy {
            model_answering: self.running || self.active_turn_handle.is_some(),
            stopping: self.cancel_requested.is_some(),
            subagents: self.agents.running(),
            background_tasks: cx.tools_arc.shells().running_count(),
            compacting: self.compacting,
            queued_commands: self.queued_commands.len(),
        })
    }

    /// Once due, with no turn running and nothing else in flight.
    pub(crate) fn start_recap_if_due(&mut self, cx: &LoopCtx<'_>) {
        if self.recap_due.is_none_or(|due| std::time::Instant::now() < due) {
            return;
        }
        // Postponed, not dropped: the recap is still wanted, only not yet, and
        // the conversation it would describe keeps growing. Clearing the timer
        // here meant one moment of work in flight — a child finishing, a shell
        // task starting — cost the recap for the rest of the session, since
        // nothing else arms it except the end of a turn.
        if self.recap_blocked_by(cx).is_some() {
            self.recap_due = Some(std::time::Instant::now() + recap_idle());
            return;
        }
        self.recap_due = None;
        let source_bg = cx.source.clone();
        let tx_bg = cx.tx.clone();
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
            // Messages not yet delivered stay pinned; the turn's end puts them back
            // in the prompt instead of dropping them.
            self.active_steer_tx = None;
            self.notice("Interrupting…");
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
    /// The prompt names the model and the thinking mode, and F3, F4 and
    /// `/provider` change them: "do not think" must not outlive a switch to
    /// high effort. Rebuilt only on a change, since the system prompt is the
    /// cached prefix.
    pub(crate) fn sync_system_prompt(&mut self) {
        if self.prompt_config.model.as_deref() != Some(self.current_model.as_str())
            || self.prompt_config.effort.as_deref() != Some(self.current_effort.as_str())
        {
            self.prompt_config = self.prompt_config.clone().with_model(&self.current_model).with_effort(&self.current_effort);
            self.apply_personality();
        }
    }

    pub(crate) fn start_turn(&mut self, cx: &LoopCtx<'_>, budgets: GoalBudgets) {
        self.sync_system_prompt();
        // Here, not only for typed prompts: a /goal, a skill or Ctrl+R measured
        // their growth from a turn long gone and compacted too early.
        self.context_before_turn = self.context_usage.total_used();
        update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
        cx.cancel.store(false, Ordering::Relaxed);
        self.suggested_prompt = None;
        self.custom_placeholder = None;
        self.running = true;
        // On disk before the model is asked anything. A machine that loses power
        // mid-turn used to lose the whole of it, because the only write came
        // after it ended: the prompt was in the file nowhere at all.
        self.autosave(cx.session_id, cx.cwd_display);
        // A warm-up still in flight would prefill the same prefix the turn is
        // about to prefill; on a one-slot server that is the wait doubled.
        self.cancel_warm_prompt_cache();
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
        self.pending_steers.clear();
        // Notices that waited (a draft, an Esc) ride along with this turn.
        self.task_inbox.turn_started();
        for notice in self.task_inbox.send_into_turn() {
            let _ = steer_tx.send(flashagent_core::Steer::text(notice));
        }
        self.active_steer_tx = Some(steer_tx);
        self.cancel_recap();
        // A long task fills the window between its own steps; the same threshold
        // that summarizes between prompts summarizes here.
        let compactor = (self.config.auto_compact_context && self.context_usage.total_capacity > 0).then(|| {
            Arc::new(TurnCompactor {
                source: cx.source.clone(),
                archive: compaction_archive_path(cx.session_id),
                memory: cx.memory_block.to_string(),
                capacity: self.context_usage.total_capacity,
                fixed: self.context_usage.system_tokens + self.context_usage.memory_tokens + self.context_usage.tools_tokens,
                threshold_pct: flashagent_core::resolved_compact_threshold(
                    self.config.context_compact_threshold,
                    self.context_usage.total_capacity,
                ),
            })
        });
        // The turn works on a handle the app also holds, so a save taken while it
        // runs writes what the turn has actually done so far. It used to hand
        // over a clone and keep its own, which is why a crash mid-turn lost the
        // whole turn and not the last few seconds of it: the only copy on disk
        // was the one written before the turn started.
        let shared = flashagent_core::loop_::SharedHistory::new(self.history.clone());
        self.turn_history = Some(shared.clone());
        self.active_turn_handle = Some(spawn_turn(
            cx.cancel.clone(),
            cx.source.clone(),
            cx.perm,
            shared,
            budgets,
            turn_opts,
            cx.tx.clone(),
            steer_rx,
            self.turn_counter,
            self.config.personality.voice_prelude(),
            compactor,
        ));
    }

    /// Ctrl+R and /regenerate. False when there is no prompt to answer again.
    pub(crate) fn regenerate(&mut self, cx: &LoopCtx<'_>) -> bool {
        let Some(user_idx) = self.history.iter().rposition(flashagent_core::is_prompt) else {
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

    /// Stops the turn clock, keeping how long it ran. Reading the instant
    /// after it is cleared is how the duration came to be `None` in every
    /// live run while the tests that use it passed: the two were written on
    /// either side of the clear and the gap was invisible.
    pub(crate) fn stop_turn_clock(&mut self) {
        self.turn_elapsed = elapsed(&mut self.turn_started);
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
        self.stop_turn_clock();
        self.active_turn_handle = None;
        self.active_steer_tx = None;
        self.task_inbox.turn_ended(matches!(res, Ok((_, DoneReason::Cancelled))));
        self.flush_task_lines();
        // A steer typed as the turn ended never reached the model; it goes back into
        // the prompt, with any picture it carried, which used to be dropped here
        // and lost for good.
        let unsent = std::mem::take(&mut self.pending_steers);
        if !unsent.is_empty() {
            // Ahead of a draft typed since: that draft is newer.
            let draft = self.input.text().to_string();
            let restored = unsent.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>().join("\n");
            for (_, pictures) in unsent {
                self.attachments.extend(pictures);
            }
            self.input.set(if draft.trim().is_empty() { restored } else { format!("{restored}\n{draft}") });
            self.background = Some(BackgroundNotice::fading(
                "The turn ended before your message reached it · Enter sends it now",
                8,
            ));
        }
        self.cancel_requested = None;
        cx.cancel.store(false, Ordering::Relaxed);
        self.token_tracker.on_finished();
        self.flash_turn_end(res.as_ref().ok().map(|(_, reason)| *reason));

        // The server just answered, so it is not silent, whatever it managed to
        // say about itself at startup. A server behind a proxy that will not
        // list its models used to leave the mascot saying "No answer from" for
        // the whole session, over a server that was answering every question,
        // and it took switching providers to clear it. Reported live.
        if matches!(res, Ok((_, DoneReason::Completed))) {
            self.server_silent = false;
        }

        // Cost against output is the evidence for whether auto guessed right. Goal
        // runs are excluded: their effort is the user's. This turn's growth predicts
        // the next one's.
        let grown = self.context_usage.total_used().saturating_sub(self.context_before_turn);
        if grown > 0 {
            self.last_turn_growth = grown.max(self.last_turn_growth / 2);
        }
        if self.goal_state.is_none() {
            // How long this turn took, and what it generated: the only way to
            // tell a model that is overthinking from one that is right on time.
            self.turn_outcome.secs = self.turn_elapsed;
            self.turn_outcome.completion_tokens = self.token_tracker.total_model_tokens;
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
                "Goal \"{}\" finished · back to {} mode, thinking {}",
                flashagent_tui::truncate_middle(&saved.task, 40),
                saved.mode.label(),
                saved.effort
            ));
        }

        match res {
            Ok((h, reason)) => {
                self.history = h;
                self.turn_history = None;
                // A style picked while the turn ran was applied to the history it replaced.
                self.apply_personality();
                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                // The turn just read all of it. A later change (interrupted turn closed,
                // compaction) makes a new prefix, which is warmed.
                self.note_prompt_cached(cx.source, cx.perm, cx.memory_block);
                self.renderer.request_reprint();

                if reason == DoneReason::Cancelled {
                    flashagent_core::mark_cut_short(&mut self.history, flashagent_core::CutShort::ByUser);
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
                    // Only turns before the last are summarized: a single long turn (a
                    // /goal run) has nothing to compact, and saying it failed misleads.
                    let earlier_turns = self.history.iter().filter(|m| flashagent_core::is_prompt(m)).count() > 1;
                    if verdict.should() && earlier_turns {
                        // In the transcript: it changes what the model remembers.
                        self.chat.push_system("Compacting context…");
                        self.renderer.request_reprint();
                        // Drawn before the wait: the command is already gone from the composer.
                        self.draw(cx, None);
                        let source_compact = cx.source.clone();
                        let before = self.context_usage.total_used();
                        let compacted = if let Some(archive) = compaction_archive_path(cx.session_id) {
                            compact_context(source_compact.as_ref(), &mut self.history, None, &archive, Some(before)).await
                        } else {
                            None
                        };
                        match compacted {
                            Some(_) => {
                                self.chat.forget_counted_context();
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
                self.turn_history = None;
                self.apply_personality();
                flashagent_core::mark_cut_short(&mut self.history, flashagent_core::CutShort::ByError);
                // The steps that did run changed files, and this note is what tells
                // the next turn to check them. Losing power here is the one moment
                // that note has to survive, so it goes to disk with the history.
                self.autosave(cx.session_id, cx.cwd_display);
                update_context_usage(&mut self.context_usage, &self.history, cx.memory_block, &self.chat, cx.perm);
                self.chat.on_event(&LoopEvent::Done(DoneReason::Failed));
                // Plain explanation first, the raw text underneath. A connection that
                // dropped is told apart from a server that refused: only the first one
                // can leave a half-written answer on screen, and that is what the user
                // is looking at when they read the rest of this.
                let cut_off = cut_off_notice(&e, self.token_tracker.turn_output());
                let explained = flashagent_tui::backend_error::explain(
                    &e,
                    &cx.source.0.base_url(),
                    &self.current_model,
                );
                let headline = match cut_off {
                    Some(why) => why,
                    None => explained.headline.clone(),
                };
                self.chat.push_line(LineKind::ToolError, headline);
                if let Some(hint) = &explained.hint {
                    // Its own line: the renderer clips embedded newlines instead of wrapping.
                    self.chat.push_line(
                        LineKind::System,
                        format!("  \x1b[38;2;160;155;145m{hint}\x1b[0m"),
                    );
                }
                if !explained.raw.trim().is_empty() && explained.headline != explained.raw.trim() {
                    self.chat.push_line(
                        LineKind::System,
                        format!("  \x1b[38;2;120;115;110m{}\x1b[0m", explained.raw.trim()),
                    );
                }
                self.notice("Ctrl+R retries the last prompt");
            }
        }
        self.run_queued_commands(cx).await;
        // Notices that came as the turn ended.
        self.deliver_task_notices(cx);

        Flow::Next
    }
}

/// Whether a failed turn ended because the connection went away rather than
/// because the server said no. Only the first can leave an answer on screen
/// that stops in the middle of a sentence, and that is the one the user has to
/// be told about rather than left to infer from a half paragraph.
///
/// Matched on the text because the error arrives as one: `LoopError` is
/// displayed into the string the loop sends, and the llm crate reports a stream
/// that closed without a finish reason as `stream interrupted: …`.
pub(crate) fn is_transport_cut(raw: &str) -> bool {
    let lower = raw.to_lowercase();
    [
        // The llm crate's own wording for a stream that ended with no reason.
        "closed before saying why",
        "stream interrupted",
        // reqwest and the transports under it.
        "connection reset",
        "connection closed",
        "connection aborted",
        "error sending request",
        "broken pipe",
        "body stream",
        "incomplete message",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

/// What a turn cut off by a dropped connection is told: that it was cut off,
/// and how much of it arrived. The count is the part that matters — without it
/// "it failed" reads as though nothing was written, and the text that is on
/// screen looks like the whole answer. `None` for every other failure, which
/// said no before there was anything to cut.
pub(crate) fn cut_off_notice(raw: &str, arrived: usize) -> Option<String> {
    if !is_transport_cut(raw) {
        return None;
    }
    Some(if arrived == 0 {
        "The connection to the model server dropped before the answer began \u{b7} nothing of this turn was written".to_string()
    } else {
        format!(
            "The connection to the model server dropped mid-answer \u{b7} about {} arrived and the answer above is cut off, not finished",
            flashagent_tui::goal::human_count(arrived as i64)
        )
    })
}

/// What is still in flight, as far as a recap is concerned.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RecapBusy {
    /// A turn is running, or one has a handle and has not reported back.
    pub(crate) model_answering: bool,
    /// Esc was pressed and the loop has not wound down yet.
    pub(crate) stopping: bool,
    pub(crate) subagents: usize,
    pub(crate) background_tasks: usize,
    pub(crate) compacting: bool,
    pub(crate) queued_commands: usize,
}

/// Why a recap may not be written right now, if it may not. A recap is one
/// whole extra request to the model, spent on a conversation that is still
/// moving: summarising work in progress describes a state that lasts no longer
/// than the summary.
///
/// `turn_phase` is deliberately not one of these. It has no idle variant, so a
/// phase left over from the last turn would read as "the model is busy" for the
/// rest of the session and no recap would ever be written again. What says the
/// model is answering is the turn itself: the running flag, the handle it runs
/// on, and a cancel it has not yet obeyed.
pub(crate) fn recap_blocked_by(busy: RecapBusy) -> Option<&'static str> {
    if busy.model_answering {
        return Some("the model is answering");
    }
    if busy.stopping {
        return Some("the last turn is still stopping");
    }
    if busy.subagents > 0 {
        return Some("subagents are working");
    }
    if busy.background_tasks > 0 {
        return Some("a background task is running");
    }
    if busy.compacting {
        return Some("the context is being compacted");
    }
    if busy.queued_commands > 0 {
        return Some("a queued command is about to start a turn");
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dropped_connection_says_the_answer_was_cut_off_and_how_much_arrived() {
        // The wording the llm crate now uses when a stream ends with no finish
        // reason, wrapped the way LoopError displays it.
        let raw = "llm: stream interrupted: the server closed before saying why";
        assert!(is_transport_cut(raw), "not recognised as a transport cut: {raw}");
        let said = cut_off_notice(raw, 812).expect("a cut-off answer gets its own line");
        assert!(said.contains("cut off"), "it does not say it was cut off: {said}");
        assert!(said.contains("812"), "it does not say how much arrived: {said}");

        // Nothing arrived is a different sentence: "cut off" would describe a
        // paragraph that is not on screen.
        let empty = cut_off_notice(raw, 0).unwrap();
        assert!(empty.contains("before the answer began"), "{empty}");
        assert!(!empty.contains("cut off"), "{empty}");
    }

    #[test]
    fn a_server_that_refused_is_not_reported_as_a_cut_off_answer() {
        // A rejected key, a full context, a rate limit: nothing was streaming, so
        // calling it a half-written answer would be a lie in the other direction.
        for raw in [
            "llm: backend returned 401: unauthorized",
            "llm: backend returned 429: rate limit exceeded",
            "llm: forbidden: Gemini refused the request (SAFETY)",
            "llm: backend returned 404: {\"error\":{\"message\":\"model_not_found\"}}",
        ] {
            assert!(!is_transport_cut(raw), "wrongly a cut off: {raw}");
            assert_eq!(cut_off_notice(raw, 400), None, "{raw}");
        }
    }

    #[test]
    fn the_other_ways_a_connection_dies_are_a_cut_off_too() {
        for raw in [
            "llm: stream interrupted: connection reset by peer",
            "llm: http: error sending request for url (http://localhost:1234/v1/chat/completions)",
            "llm: stream interrupted: body stream ended unexpectedly",
        ] {
            assert!(is_transport_cut(raw), "not recognised: {raw}");
        }
    }

    #[test]
    fn a_recap_waits_for_the_work_in_flight_and_does_not_wait_for_nothing() {
        let idle = RecapBusy::default();
        assert_eq!(recap_blocked_by(idle), None, "an idle conversation gets its recap");

        // Each of the three the user named, on its own.
        let subagent = RecapBusy { subagents: 2, ..idle };
        assert_eq!(recap_blocked_by(subagent), Some("subagents are working"));
        let task = RecapBusy { background_tasks: 1, ..idle };
        assert_eq!(recap_blocked_by(task), Some("a background task is running"));
        let answering = RecapBusy { model_answering: true, ..idle };
        assert_eq!(recap_blocked_by(answering), Some("the model is answering"));

        // And the rest of the in-flight states that make a recap wrong the same way.
        for (busy, why) in [
            (RecapBusy { stopping: true, ..idle }, "the last turn is still stopping"),
            (RecapBusy { compacting: true, ..idle }, "the context is being compacted"),
            (RecapBusy { queued_commands: 1, ..idle }, "a queued command is about to start a turn"),
        ] {
            assert_eq!(recap_blocked_by(busy), Some(why));
        }
    }

    #[test]
    fn the_model_phase_is_not_what_says_the_model_is_busy() {
        // A recap gate built on turn_phase never opens again: the phase has no
        // idle variant, so whatever the last turn ended on reads as "busy" for
        // the rest of the session and no recap is ever written. Only the turn
        // itself says so, and it does.
        let busy = RecapBusy { model_answering: false, ..RecapBusy::default() };
        assert_eq!(recap_blocked_by(busy), None, "a phase left over from a finished turn must not block");
    }
}
