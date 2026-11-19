use super::*;
use flashagent_tui::i18n::{tr, tr_fill};

/// The tools the prompt talks about, and whether they are on offer.
///
/// The prompt describes `agent` and `user_interaction`, and the owner can
/// switch either off. A prompt that describes a tool the model was not
/// offered sends it looking for something that is not there, and a turn is
/// spent on the refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OfferedTools {
    pub subagents: bool,
    pub ask_user: bool,
}

impl OfferedTools {
    /// What the model can call right now.
    pub(crate) fn from_specs(specs: &[flashagent_llm::ToolSpec]) -> Self {
        let has = |name: &str| specs.iter().any(|s| s.name == name);
        Self { subagents: has("agent"), ask_user: has("user_interaction") }
    }
}

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
    /// the next question up. It is written only once the user has gone quiet,
    /// except on the first turn: that call also names the session, and the
    /// picker shows the conversation under that name from the start. The name
    /// is taken from the opening exchange even if this call runs much later.
    pub(crate) fn schedule_recap(&mut self) {
        self.cancel_recap();
        let first_turn = self.history.iter().filter(|m| flashagent_core::is_prompt(m)).count() <= 1;
        self.recap_due = Some(std::time::Instant::now() + if first_turn { std::time::Duration::ZERO } else { recap_idle() });
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
        // here meant one moment of work in flight (a child finishing, a shell
        // task starting) cost the recap for the rest of the session, since
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
        // The answer may land after the user has switched to another session;
        // without the id it would name and annotate whatever is on screen then.
        let session = cx.session_id.clone();
        self.recap_task = Some(tokio::spawn(async move {
            if let Some((llm_recap, llm_suggestion, llm_title)) = generate_llm_recap_and_suggestion(&source_bg, &history_bg).await {
                let _ = tx_bg.send(UiEvent::BackgroundRecap {
                    turn_id,
                    session,
                    recap: llm_recap,
                    suggestion: llm_suggestion,
                    title: llm_title,
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
            self.notice(tr("Interrupting…"));
        }
        self.renderer.request_reprint();
    }

    /// Keeps a compacted summary at the end of the system prompt.
    pub(crate) fn apply_personality(&mut self) {
        let base = build_system_prompt(
            &self
                .prompt_config
                .clone()
                .with_personality(&self.config.personality)
                .with_goal_mode(self.goal_active),
        );
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
    ///
    /// The prompt also names the tools that are on offer, and the owner can
    /// switch them off in `/tools` while the app is running. So the set of
    /// tools is part of what is compared: a prompt that still offers a
    /// subagent the owner has just switched off sends the model looking for
    /// something that is not there.
    pub(crate) fn sync_system_prompt(&mut self, offered: OfferedTools) {
        if self.prompt_config.model.as_deref() != Some(self.current_model.as_str())
            || self.prompt_config.effort.as_deref() != Some(self.current_effort.as_str())
            || self.prompt_config.subagents_available != offered.subagents
            || self.prompt_config.ask_user_available != offered.ask_user
        {
            self.prompt_config = self
                .prompt_config
                .clone()
                .with_model(&self.current_model)
                .with_effort(&self.current_effort)
                .with_subagents(offered.subagents)
                .with_ask_user(offered.ask_user);
            self.apply_personality();
        }
    }

    /// Where the live context line used to start in older saved sessions.
    /// New prompts never carry one: a line that moves would move the whole
    /// cached prefix with it.
    pub(crate) const CONTEXT_LINE_MARK: &str = "\n\n[Context]";


/// The prompt tier follows the model actually running. A switch from a local
/// 16k model to a cloud 200k one should not keep the compact prompt, and the
/// reverse should not send four thousand tokens to a 3B model. The level is
/// stable between switches, so the cached prefix stays put.
pub(crate) fn refresh_prompt_tier(&mut self, cx: &LoopCtx<'_>) {
        let local = crate::backend_is_local(cx.source);
        let reasoning = crate::model_reasoning_supported(cx.source, &self.current_model);
        let detail = flashagent_core::PromptDetail::auto(local, self.context_usage.total_capacity, &self.config.prompt_detail);
        if self.prompt_config.detail != detail
            || self.prompt_config.reasoning_supported != reasoning
            || self.prompt_config.local != local
        {
            self.prompt_config.detail = detail;
            self.prompt_config.reasoning_supported = reasoning;
            self.prompt_config.local = local;
            self.apply_personality();
        }
    }

pub(crate) fn start_turn(&mut self, cx: &LoopCtx<'_>, budgets: GoalBudgets) {
        if self.running || self.compacting { return; }
        self.refresh_prompt_tier(cx);
        self.sync_system_prompt(OfferedTools::from_specs(&cx.perm.specs()));
        // Here, not only for typed prompts: a /goal, a skill or Ctrl+R measured
        // their growth from a turn long gone and compacted too early.
        self.context_before_turn = self.context_usage.total_used();
        self.context_usage.invalidate_server();
        update_context_usage(&self.current_model, &mut self.context_usage, &self.history, &self.memory_block, &self.chat, cx.perm);
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
        // The last server count described the request before this one. A turn
        // that regenerates has just cut the history, so carrying it over would
        // show a window fuller than the prompt about to be sent; the first
        // usage of this turn puts the fresh figure in.
        self.context_request_tokens = self.context_usage.local_used();
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
        let report = Arc::new(std::sync::Mutex::new(None));
        let compactor = (self.config.auto_compact_context && self.context_usage.total_capacity > 0).then(|| {
            Arc::new(TurnCompactor {
                source: cx.source.clone(),
                archive: session_path(cx.session_id),
                capacity: self.context_usage.total_capacity,
                fixed: self.context_usage.system_tokens + self.context_usage.memory_tokens + self.context_usage.tools_tokens,
                threshold_pct: flashagent_core::resolved_compact_threshold(
                    self.config.context_compact_threshold,
                    self.context_usage.total_capacity,
                ),
                report: report.clone(),
                sink: Some(SummarySink::ui(cx.tx.clone())),
                model: self.current_model.clone(),
            })
        });
        self.mid_turn_report = compactor.is_some().then_some(report);
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
            compactor,
        ));
    }

    /// A second turn cannot start while the first runs: the handle would be
    /// dropped and the transcript cut under the answer being written. The
    /// welcome card's click reached this path without the key's own check.
    pub(crate) fn may_regenerate(&mut self) -> bool {
        if self.running {
            self.say(BackgroundNotice::fading(
                tr("An answer is still being written · Esc stops it now").to_string(),
                4,
            ));
            return false;
        }
        true
    }

    /// A new user-initiated turn: a reconnect timer from the failed turn must
    /// not fire into it.
    pub(crate) fn clear_retry_state(&mut self) {
        self.retry_after_error = false;
        self.auto_retry_attempt = 0;
        self.retry_generation = self.retry_generation.wrapping_add(1);
        self.reconnecting = None;
    }

    /// Continue a turn that stopped at an error, keeping everything that ran
    /// before it. The note is a user message because the API needs one after a
    /// partial assistant answer; it names what not to repeat.
    pub(crate) fn resume_failed_turn(&mut self, cx: &LoopCtx<'_>) -> bool {
        if self.running || self.compacting || self.history.is_empty() {
            return false;
        }
        self.retry_generation = self.retry_generation.wrapping_add(1);
        self.reconnecting = None;
        self.retry_after_error = false;
        self.history.push(flashagent_llm::ChatMessage::user(
            "[The previous attempt stopped here because of a connection error. Continue from this point. Do not repeat a tool call whose result is already above; use that result. If the last step failed, correct it and continue. Do not start over.]"
        ));
        self.chat.push_system(tr("Continuing from the failed step"));
        self.flush_switch_notice();
        self.renderer.scroll_to_bottom();
        self.last_expanded = false;
        self.start_turn(cx, GoalBudgets::steps_only(self.max_steps));
        true
    }

    /// Ctrl+R and /regenerate. False when there is no prompt to answer again.
    pub(crate) fn regenerate(&mut self, cx: &LoopCtx<'_>) -> bool {
        if !self.may_regenerate() {
            return false;
        }
        if self.retry_after_error {
            // A failed turn kept its completed tool calls and partial answer.
            // Repeating the whole prompt would throw that work away and run the
            // same tools twice; resume from the point that failed instead.
            self.auto_retry_attempt = 0;
            return self.resume_failed_turn(cx);
        }
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
        // The retry is the first request to use a switch made since the last
        // answer; the rule goes below the prompt it answers again.
        self.flush_switch_notice();
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

    /// The app's own half of the handback: the flag that says a goal run is
    /// under way, the effort and the step limit the run took over.
    ///
    /// The prompt is built from `goal_active`, and a flag left on tells the
    /// model for the rest of the session that there is no user to answer it.
    pub(crate) fn take_goal(&mut self) -> Option<SavedGoalState> {
        let saved = self.goal_state.take()?;
        self.goal_active = false;
        self.current_effort = saved.effort.clone();
        self.max_steps = saved.max_steps;
        Some(saved)
    }

    /// The run is over: the mode, the effort and the step limit the goal took
    /// over go back to what they were, and the prompt stops saying that an
    /// autonomous run is under way.
    ///
    /// One place for every ending (a turn that finished, a turn that failed,
    /// a turn that would not wind down), so a goal can never leave the app in
    /// a mode the owner did not choose. The prompt matters most here: it is
    /// rebuilt with `goal_active`, and a flag left on says to the model for
    /// the rest of the session that there is no user to answer it.
    ///
    /// False when no goal was running.
    pub(crate) fn end_goal(&mut self, cx: &LoopCtx<'_>, reason: DoneReason) -> bool {
        let Some(saved) = self.take_goal() else {
            return false;
        };
        cx.tools_arc.set_goal_mode(false);
        cx.perm.state().set_goal_active(false);
        cx.perm.state().set_mode(saved.mode);
        self.refresh_welcome_state(cx.source, cx.mascot_mood);
        if let Some(ledger) = self.goal_ledger.take() {
            push_goal_report(&mut self.chat, &ledger, reason);
        }
        let task = flashagent_tui::truncate_middle(&saved.task, 40);
        self.notice(tr_fill(
            "Goal \"{task}\" finished · back to {mode} mode, thinking {effort}",
            &[("task", task.as_str()), ("mode", saved.mode.label()), ("effort", &saved.effort)],
        ));
        true
    }

    pub(crate) fn restore_unsent_steering(&mut self) {
        let unsent = std::mem::take(&mut self.pending_steers);
        if unsent.is_empty() { return; }
        let draft = self.input.text().to_string();
        let restored = unsent.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>().join("\n");
        for (_, pictures) in unsent { self.attachments.extend(pictures); }
        self.input.set(if draft.trim().is_empty() { restored } else { format!("{restored}\n{draft}") });
        self.say(BackgroundNotice::fading(tr("The turn ended before your message reached it · Enter sends it now"), 8));
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
        // What the models were measured to be goes onto the disk at the end of
        // the turn: a model that was measured in this turn is not measured
        // again in the next one.
        if let Some(path) = flashagent_llm::measurements_path() {
            flashagent_llm::save_measurements(&path);
        }
        // The lifetime counter takes the same point in the turn.
        self.flush_usage_history();
        // A turn can end while its mid-turn compaction is still writing (Esc,
        // a failed request). The card promised a summary, so it is withdrawn
        // rather than left spinning in a transcript that will never change.
        self.chat.abandon_compaction_card(tr("the turn ended before the summary landed"));
        self.mid_turn_report = None;
        self.task_inbox.turn_ended(matches!(res, Ok((_, DoneReason::Cancelled))));
        self.flush_task_lines();
        // A steer typed as the turn ended never reached the model; it goes back into
        // the prompt, with any picture it carried, which used to be dropped here
        // and lost for good.
        self.restore_unsent_steering();
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

        // The goal ends the same way whether the turn finished or failed: the
        // mode, the effort and the limits the run took over go back to what
        // they were, and the prompt stops saying that an autonomous run is
        // under way. A goal that failed is a goal that ended: the app must
        // not stay in Accept All with the goal's limits.
        let goal_reason = match &res {
            Ok((_, reason)) => *reason,
            Err(_) => DoneReason::Failed,
        };
        self.end_goal(cx, goal_reason);

        match res {
            Ok((h, reason)) => {
                self.history = h;
                self.turn_history = None;
                self.retry_after_error = false;
                self.auto_retry_attempt = 0;
                self.reconnecting = None;
                self.retry_generation = self.retry_generation.wrapping_add(1);
                // A style picked while the turn ran was applied to the history it replaced.
                self.apply_personality();
                update_context_usage(&self.current_model, &mut self.context_usage, &self.history, &self.memory_block, &self.chat, cx.perm);
                // The turn just read all of it. A later change (interrupted turn closed,
                // compaction) makes a new prefix, which is warmed.
                self.note_prompt_cached(cx.source, cx.perm);
                self.renderer.request_reprint();

                if reason == DoneReason::Cancelled {
                    flashagent_core::mark_cut_short(&mut self.history, flashagent_core::CutShort::ByUser);
                    self.suggested_prompt = None;
                    let interrupt_msg = flashagent_tui::i18n::tr("Request interrupted by user");
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
                        self.compact_command_with_growth(cx, "", false);
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
                self.retry_after_error = true;
                self.apply_personality();
                flashagent_core::mark_cut_short(&mut self.history, flashagent_core::CutShort::ByError);
                // The steps that did run changed files, and this note is what tells
                // the next turn to check them. Losing power here is the one moment
                // that note has to survive, so it goes to disk with the history.
                self.autosave(cx.session_id, cx.cwd_display);
                update_context_usage(&self.current_model, &mut self.context_usage, &self.history, &self.memory_block, &self.chat, cx.perm);
                self.chat.on_event(&LoopEvent::Done(DoneReason::Failed));
                // A dropped connection gets a few silent reconnects. It resumes
                // from the failure point, so a tool that already ran is not run
                // again; the status line says which attempt this is.
                let max = self.config.network_retries;
                if max > 0 && self.auto_retry_attempt < max && retryable_error(&e) {
                    self.auto_retry_attempt += 1;
                    self.retry_generation = self.retry_generation.wrapping_add(1);
                    let generation = self.retry_generation;
                    self.reconnecting = Some((self.auto_retry_attempt, max));
                    self.chat.push_line(
                        LineKind::System,
                        format!("  \x1b[38;2;245;160;80m{}\x1b[0m", tr_fill(
                            "Connection lost · reconnecting {n}/{max}…",
                            &[("n", &self.auto_retry_attempt.to_string()), ("max", &max.to_string())],
                        )),
                    );
                    let delay = std::time::Duration::from_secs(1u64 << (self.auto_retry_attempt.saturating_sub(1).min(4)));
                    let tx = cx.tx.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(delay).await;
                        let _ = tx.send(UiEvent::Retry { generation });
                    });
                    self.renderer.request_reprint();
                    self.run_queued_commands(cx).await;
                    self.deliver_task_notices(cx);
                    return Flow::Next;
                }
                self.reconnecting = None;
                self.auto_retry_attempt = 0;
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
                self.notice(tr("Ctrl+R continues from the failed step"));
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
/// and how much of it arrived. The count is the part that matters: without it
/// "it failed" reads as though nothing was written, and the text that is on
/// screen looks like the whole answer. `None` for every other failure, which
/// said no before there was anything to cut.
/// Errors a reconnect can plausibly fix. A bad key, a missing model or a
/// context that is too long is not one: retrying them only spends time.
fn retryable_error(raw: &str) -> bool {
    if is_transport_cut(raw) {
        return true;
    }
    let lower = raw.to_lowercase();
    if ["401", "403", "404", "invalid api key", "model not found", "context length", "too long", "content policy"]
        .iter()
        .any(|m| lower.contains(m))
    {
        return false;
    }
    [
        "429", "rate limit", "too many requests", "502", "503", "504", "529", "overloaded",
        "temporarily unavailable", "timed out", "timeout", "unreachable", "dns", "connect",
    ]
    .iter()
    .any(|m| lower.contains(m))
}

pub(crate) fn cut_off_notice(raw: &str, arrived: usize) -> Option<String> {
    if !is_transport_cut(raw) {
        return None;
    }
    Some(if arrived == 0 {
        flashagent_tui::i18n::tr("The connection to the model server dropped before the answer began \u{b7} nothing of this turn was written")
            .to_string()
    } else {
        tr_fill(
            "The connection to the model server dropped mid-answer \u{b7} about {arrived} arrived and the answer above is cut off, not finished",
            &[("arrived", &flashagent_tui::goal::human_count(arrived as i64))],
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
    fn a_dropped_connection_retries_and_a_bad_key_does_not() {
        assert!(retryable_error("stream interrupted: the server closed the connection"));
        assert!(retryable_error("HTTP 429: rate limit exceeded"));
        assert!(retryable_error("503 Service Unavailable: overloaded gateway"));
        assert!(!retryable_error("401 Unauthorized: invalid api key"));
        assert!(!retryable_error("model not found: no-such-model"));
        assert!(!retryable_error("context length exceeded: the prompt is too long"));
    }

    #[test]
    fn a_second_regenerate_cannot_start_while_the_first_answer_is_written() {
        let mut app = crate::tests::test_app();
        app.running = true;
        assert!(!app.may_regenerate(), "a second turn started under the one being written");
        app.running = false;
        assert!(app.may_regenerate());
    }

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
