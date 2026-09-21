use super::*;

/// How soon a second Esc on an empty prompt has to follow the first to quit.
pub(crate) const ESC_QUIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(2);

impl App {
    /// The next launch starts in the mode the user is in now — whichever it
    /// is, Accept All included. A `/goal` run's Accept All is its own and is
    /// never remembered; the mode it hands back already was.
    pub(crate) fn remember_mode(&mut self, mode: PermissionMode) {
        if self.goal_state.is_some() || self.config.permission_mode == mode {
            return;
        }
        self.config.permission_mode = mode;
        self.save_config();
    }

    /// Write the settings to disk, and say so when that fails: a setting
    /// that silently does not stick is found out only on the next launch.
    pub(crate) fn save_config(&mut self) {
        if let Err(e) = self.config.save() {
            self.notice(format!("Settings not saved: {e}"));
        }
    }

    /// Ctrl+U and /update: show the update that is already under way —
    /// started in the background before anyone asked — or start one that
    /// checks, downloads and installs in one go.
    pub(crate) fn start_or_watch_update(&mut self, cx: &LoopCtx<'_>) {
        let action = update_key_action(
            flashagent_svc::updater::is_dev_mode(),
            self.update_progress.is_some(),
            cx.update_busy.load(Ordering::SeqCst),
            self.pending_update.is_some(),
        );
        self.renderer.request_reprint();
        let checking = |channel: flashagent_core::config::UpdateChannel| {
            BackgroundNotice::fading(format!("{UPDATE_LINE_PREFIX}\u{b7} checking the {} channel...", channel.label()), 30)
        };
        match action {
            UpdateKeyAction::DevMode => {
                self.background = Some(BackgroundNotice::fading("Auto-updater is disabled in dev mode", 6));
                return;
            }
            UpdateKeyAction::ShowProgress => {
                self.update_watched = true;
                if let Some((version, stage)) = &self.update_progress {
                    self.background = Some(BackgroundNotice::sticky(update_progress_line(version, *stage)));
                }
                return;
            }
            UpdateKeyAction::WatchCheck => {
                self.update_watched = true;
                self.background = Some(checking(self.config.update_channel));
                return;
            }
            UpdateKeyAction::InstallPending | UpdateKeyAction::CheckAndInstall => {}
        }
        // Claimed here, not just read above: the background updater may have
        // started in between.
        if cx.update_busy.swap(true, Ordering::SeqCst) {
            self.update_watched = true;
            self.background = Some(checking(self.config.update_channel));
            return;
        }
        self.update_watched = true;
        let update_tx = cx.update_tx.clone();
        let busy = cx.update_busy.clone();
        let pending = if action == UpdateKeyAction::InstallPending { self.pending_update.clone() } else { None };
        self.background = Some(match &pending {
            Some((version, ..)) => BackgroundNotice::sticky(format!("{UPDATE_LINE_PREFIX}{version} \u{b7} starting download...")),
            None => checking(self.config.update_channel),
        });
        let channel = self.config.update_channel;
        tokio::spawn(async move {
            let target = match pending {
                Some(found) => Ok(Some(found)),
                None => match flashagent_svc::updater::check_for_updates(channel, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                    Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                        Ok(Some((target, asset_name, download_url, checksums_url)))
                    }
                    Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => Err(UpdateNotice::UpToDate { version: current }),
                    Err(e) => Err(UpdateNotice::Failed { error: e.to_string() }),
                },
            };
            let notice = match target {
                Ok(Some((version, asset_name, download_url, checksums_url))) => {
                    let progress_tx = update_tx.clone();
                    let for_progress = version.clone();
                    let result = flashagent_svc::updater::download_and_apply_with_progress(
                        &download_url,
                        &asset_name,
                        checksums_url.as_deref(),
                        move |stage| {
                            let _ = progress_tx.send(UpdateNotice::Progress { version: for_progress.clone(), stage });
                        },
                    )
                    .await;
                    match result {
                        Ok(_) => UpdateNotice::Ready { version },
                        Err(e) => UpdateNotice::Failed { error: e.to_string() },
                    }
                }
                Ok(None) => return,
                Err(notice) => notice,
            };
            busy.store(false, Ordering::SeqCst);
            let _ = update_tx.send(notice);
        });
    }

    /// After text was taken out of the prompt: the history walk is over, and
    /// an emptied prompt offers the suggestion again.
    fn edited(&mut self) {
        self.history_index = None;
        self.autocomplete_idx = 0;
        if self.input.is_empty() && self.latest_suggestion.is_some() {
            self.suggested_prompt = self.latest_suggestion.clone();
        }
    }

    /// Ctrl+F: typing narrows the search, Ctrl+F again goes further back,
    /// Enter or → takes what was found into the prompt to edit or send, Esc
    /// puts back what was there before.
    fn history_search_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        let Some(search) = self.history_search.as_mut() else { return };
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char(c) if ctrl && matches!(latin(c), 'f' | 'r') => search.older(&self.input_history),
            KeyCode::Char(c) if ctrl && latin(c) == 'g' => {
                let draft = std::mem::take(&mut search.draft);
                self.input.set(draft);
                self.history_search = None;
            }
            KeyCode::Char(c) if !ctrl && !mods.contains(KeyModifiers::ALT) => search.type_char(c),
            KeyCode::Backspace => search.backspace(),
            KeyCode::Esc => {
                let draft = std::mem::take(&mut search.draft);
                self.input.set(draft);
                self.history_search = None;
            }
            _ => {
                let found = search.found(&self.input_history).map(str::to_string);
                let draft = std::mem::take(&mut search.draft);
                self.input.set(found.unwrap_or(draft));
                self.history_search = None;
                self.history_index = None;
            }
        }
        self.renderer.request_reprint();
    }

    /// A key the overlays did not claim: typing, editing the prompt, history,
    /// function keys and shortcuts, and Enter.
    pub(crate) async fn handle_key(&mut self, cx: &mut LoopCtx<'_>, code: KeyCode, mods: KeyModifiers) -> Flow {
        if self.history_search.is_some() {
            self.history_search_key(code, mods);
            return Flow::Next;
        }
        match code {
            KeyCode::Esc => {
                if self.running {
                    self.interrupt(cx);
                } else if !self.input.is_empty() {
                    self.input.clear();
                    self.autocomplete_idx = 0;
                    self.history_index = None;
                    if self.latest_suggestion.is_some() {
                        self.suggested_prompt = self.latest_suggestion.clone();
                    }
                    self.renderer.request_reprint();
                } else if !self.attachments.is_empty() {
                    self.attachments.clear();
                    self.background = Some(BackgroundNotice::fading("Attachments cleared".to_string(), 4));
                    self.renderer.request_reprint();
                } else if self.last_esc.is_some_and(|t| t.elapsed() < ESC_QUIT_WINDOW) {
                    // The second press of a double Esc. One stray Esc used
                    // to quit outright; a session is saved either way.
                    return Flow::Quit;
                } else {
                    // The first press also clears a suggestion, so quitting
                    // right after an answer is still two presses, not three.
                    self.suggested_prompt = None;
                    self.latest_suggestion = None;
                    self.last_esc = Some(std::time::Instant::now());
                    self.custom_placeholder = Some("Press Esc again to quit".to_string());
                    self.renderer.request_reprint();
                }
            }
            // F1: toggle context modal
            KeyCode::F(1) => {
                self.open_overlay(Overlay::Context(ContextModal::new(self.context_usage.clone())));
            }

            // Ctrl+C / Ctrl+Shift+C (handles both Latin and alternate physical keycodes)
            KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                if mods.contains(KeyModifiers::CONTROL) =>
            {
                if self.running {
                    self.interrupt(cx);
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
                    self.suggested_prompt = None;
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
                    if !text.is_empty() {
                        self.input.insert_str(&text);
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
                    && self.overlay.is_none()
                    && !self.regenerate(cx)
                {
                    let now = std::time::Instant::now();
                    self.copy_toast = Some(("No previous turn to regenerate".to_string(), now));
                    self.renderer.request_reprint();
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
                        self.input.set(edited);
                        self.autocomplete_idx = 0;
                    }
                    Err(err) => {
                        self.notice(format!("Failed to launch external editor: {err}"));
                    }
                }
                self.renderer.request_reprint();
            }

            // CTRL + U: download & apply pending update or check for updates
            KeyCode::Char('u') | KeyCode::Char('U') | KeyCode::Char('\u{0433}') | KeyCode::Char('\u{0413}')
                if mods.contains(KeyModifiers::CONTROL) =>
            {
                self.start_or_watch_update(cx);
            }

            // F4 or CTRL + T or ALT + T: open Thinking Effort menu (supports alternative keyboard layouts)
            KeyCode::F(4)
            | KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Char('\u{0435}') | KeyCode::Char('\u{0415}')
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(4)) =>
            {
                self.open_effort_menu(cx.source);
            }

            // F3 or CTRL + M or ALT + M: open Model menu (supports alternative keyboard layouts)
            KeyCode::F(3)
            | KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Char('\u{044c}') | KeyCode::Char('\u{042c}')
                if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(3)) =>
            {
                self.open_model_menu(cx.source);
            }

            // F5: open Sampling Parameters menu
            KeyCode::F(5) => {
                self.open_overlay(Overlay::Sampling(SamplingView::new(&self.config)));
            }

            // Mode cycling with Shift+Tab (both KeyCode::BackTab and Tab+Shift)
            KeyCode::BackTab | KeyCode::Tab if matches!(code, KeyCode::BackTab) || mods.contains(KeyModifiers::SHIFT) => {
                let next_mode = cx.perm.state().mode().next();
                cx.perm.state().set_mode(next_mode);
                self.remember_mode(next_mode);
                self.refresh_welcome(cx.source, cx.perm.state().mode(), cx.mascot_mood);
                self.custom_placeholder = Some(format!("Permission mode set to: {}", next_mode.label()));
                self.suggested_prompt = None;
                self.renderer.request_reprint();
            }
            // Tab on empty input: toggle settings tab
            KeyCode::Tab if self.input.is_empty() && cx.gate.pending().is_none() => {
                let view = self.runtime_settings(cx.perm.state().mode());
                self.open_overlay(Overlay::Settings(Box::new(view)));
            }
            // Tab: complete autocomplete suggestion if input starts with `/`, or toggle approval choice when pending
            KeyCode::Tab if cx.gate.pending().is_some() => {
                self.confirm_select.toggle();
            }
            KeyCode::Tab if self.input.starts_with('/') => {
                if let Some(ac) = AutocompletePopup::for_input(&self.input, std::path::Path::new("."), self.autocomplete_idx) {
                    self.input.set(ac.complete_input(&self.input));
                    self.autocomplete_idx = 0;
                }
            }
            // Arrow navigation for approval card, autocomplete popup, and prompt history
            KeyCode::Left => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.left();
                } else if word_mods(mods) {
                    self.input.word_left();
                } else {
                    self.input.left();
                }
            }
            KeyCode::Right => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.right();
                } else if !self.running && self.input.is_empty() {
                    if let Some(sug) = self.suggested_prompt.take() {
                        self.latest_suggestion = None;
                        self.input.set(sug);
                        self.renderer.request_reprint();
                    }
                } else if word_mods(mods) {
                    self.input.word_right();
                } else {
                    self.input.right();
                }
            }
            KeyCode::Home if cx.gate.pending().is_none() => self.input.home(),
            KeyCode::End if cx.gate.pending().is_none() => self.input.end(),
            KeyCode::Delete if cx.gate.pending().is_none() => {
                self.input.delete();
                self.edited();
            }
            KeyCode::Up => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.toggle();
                } else if !self.running && self.input.starts_with('/') && self.input.line_count() == 1 {
                    if let Some(ac) = AutocompletePopup::for_input(&self.input, std::path::Path::new("."), self.autocomplete_idx) {
                        if self.autocomplete_idx == 0 {
                             self.autocomplete_idx = ac.items.len().saturating_sub(1);
                        } else {
                             self.autocomplete_idx -= 1;
                        }
                    }
                } else if self.input.up() {
                    // Moved within a text of several lines.
                } else if !self.running && !self.input_history.is_empty() {
                    match self.history_index {
                        None => {
                            self.current_draft = self.input.to_string();
                            let idx = self.input_history.len() - 1;
                            self.history_index = Some(idx);
                            self.input.set(self.input_history[idx].clone());
                        }
                        Some(idx) if idx > 0 => {
                            let new_idx = idx - 1;
                            self.history_index = Some(new_idx);
                            self.input.set(self.input_history[new_idx].clone());
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Down => {
                if cx.gate.pending().is_some() {
                    self.confirm_select.toggle();
                } else if !self.running && self.input.starts_with('/') && self.input.line_count() == 1 {
                    if let Some(ac) = AutocompletePopup::for_input(&self.input, std::path::Path::new("."), self.autocomplete_idx) {
                        self.autocomplete_idx = (self.autocomplete_idx + 1) % ac.items.len();
                    }
                } else if self.input.down() {
                    // Moved within a text of several lines.
                } else if !self.running {
                    if let Some(idx) = self.history_index {
                        if idx + 1 < self.input_history.len() {
                            let new_idx = idx + 1;
                            self.history_index = Some(new_idx);
                            self.input.set(self.input_history[new_idx].clone());
                        } else {
                            self.history_index = None;
                            self.input.set(std::mem::take(&mut self.current_draft));
                        }
                    }
                }
            }
            // Ctrl+Backspace arrives as Ctrl+H in many terminals, Alt+Backspace
            // as Backspace with Alt: both take a word, as in a shell.
            KeyCode::Backspace if cx.gate.pending().is_none() && word_mods(mods) => {
                self.input.delete_word_before();
                self.edited();
            }
            KeyCode::Backspace if cx.gate.pending().is_none() => {
                self.input.backspace();
                self.edited();
            }
            // A new line instead of sending: Alt+Enter, and Shift+Enter in
            // terminals that report it (most send Shift+Enter as a plain
            // Enter, and there `\` + Enter or Ctrl+J does the same).
            KeyCode::Enter
                if cx.gate.pending().is_none() && (mods.contains(KeyModifiers::ALT) || mods.contains(KeyModifiers::SHIFT)) =>
            {
                self.input.insert_char('\n');
                self.edited();
            }
            KeyCode::Enter if cx.gate.pending().is_none() && self.input[..self.input.cursor()].ends_with('\\') => {
                self.input.backspace();
                self.input.insert_char('\n');
                self.edited();
            }
            KeyCode::Enter => {
                self.custom_placeholder = None;
                self.suggested_prompt = None;
                self.latest_suggestion = None;
                if cx.gate.pending().is_some() {
                    cx.gate.respond(self.confirm_select.decision());
                    self.confirm_select = ConfirmSelect::new();
                } else if !self.input.is_empty() && self.running {
                    if let Some(steer_tx) = self.active_steer_tx.clone() {
                        let text = self.input.take();
                        self.remember_prompt(&text);
                        self.history_index = None;
                        self.current_draft.clear();
                        let _ = steer_tx.send(text.clone());
                        self.pending_steers.push(text);
                        self.renderer.request_reprint();
                    }
                } else if (!self.input.is_empty() || !self.attachments.is_empty()) && !self.running {
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
                self.input.insert_char(c);
                self.history_index = None;
                self.autocomplete_idx = 0;
            }
            // Shell editing keys, on either keyboard layout.
            KeyCode::Char(c) if cx.gate.pending().is_none() && mods.contains(KeyModifiers::CONTROL) => match latin(c) {
                'a' => self.input.home(),
                'j' => {
                    self.input.insert_char('\n');
                    self.edited();
                }
                'w' | 'h' => {
                    self.input.delete_word_before();
                    self.edited();
                }
                'k' => {
                    self.input.delete_to_line_end();
                    self.edited();
                }
                'f' if self.input_history.is_empty() => {
                    self.background = Some(BackgroundNotice::fading("No earlier prompts to search yet", 4));
                    self.renderer.request_reprint();
                }
                'f' => {
                    self.history_search = Some(flashagent_tui::HistorySearch::start(self.input.to_string()));
                    self.renderer.request_reprint();
                }
                _ => {}
            },
            KeyCode::Char(c) if cx.gate.pending().is_none() && mods.contains(KeyModifiers::ALT) => match latin(c) {
                'b' => self.input.word_left(),
                'f' => self.input.word_right(),
                _ => {}
            },
            _ => {}
        }
        Flow::Next
    }
}

/// Ctrl or Alt held with an arrow or Backspace: move or delete by words.
fn word_mods(mods: KeyModifiers) -> bool {
    mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)
}

/// The Latin letter on the same key as `c` in the Russian layout, so
/// Ctrl+W still deletes a word with the layout switched. Anything else is
/// returned lower-cased.
fn latin(c: char) -> char {
    const RU: &str = "йцукенгшщзфывапролдячсмить";
    const EN: &str = "qwertyuiopasdfghjklzxcvbnm";
    let c = c.to_lowercase().next().unwrap_or(c);
    RU.chars().position(|r| r == c).and_then(|i| EN.chars().nth(i)).unwrap_or(c)
}
