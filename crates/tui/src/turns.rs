use super::*;

impl App {
    /// A turn ended: apply its history, report how it went, compact the
    /// context if it needs it, and ask for a recap.
    pub(crate) async fn finish_turn(
        &mut self,
        cx: &mut LoopCtx<'_>,
        turn_id: u64,
        res: Result<(Vec<ChatMessage>, DoneReason), (String, Vec<ChatMessage>)>,
    ) -> Flow {
        macro_rules! notice {
            ($text:expr) => {{
                self.custom_placeholder = Some(($text).to_string());
                self.suggested_prompt.take();
                self.renderer.request_reprint();
            }};
        }
        if turn_id != self.turn_counter || self.aborted_turn == Some(turn_id) {
            // A turn that was hard-aborted (or superseded) reporting late.
            return Flow::Continue;
        }
        self.running = false;
        self.turn_started = None;
        self.active_turn_handle = None;
        self.active_steer_tx = None;
        self.cancel_requested = None;
        cx.cancel.store(false, Ordering::Relaxed);
        self.token_tracker.on_finished();

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
            notice!(&format!(
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
                            // The command that started this is gone from the
                            // composer; its suggestion list must go with it.
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
                    tokio::spawn(async move {
                        if let Some((llm_recap, llm_suggestion)) = generate_llm_recap_and_suggestion(&source_bg, &history_bg).await {
                            let _ = tx_bg.send(UiEvent::BackgroundRecap {
                                turn_id,
                                recap: llm_recap,
                                suggestion: llm_suggestion,
                            });
                        }
                    });
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
                notice!("Ctrl+R retries the last prompt");
            }
        }

        Flow::Next
    }
}
