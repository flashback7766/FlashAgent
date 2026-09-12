//! First-time launch setup wizard.
//! Guides user through backend connection, API key configuration, model selection, behavior, and sampling.

use std::io::Write;
use flashagent_core::{AppConfig, BackendPreset, PermissionMode};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// Mask API key showing first characters and last 4 characters if long enough.
pub fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    if key.len() <= 6 {
        return "•".repeat(key.len());
    }
    let prefix_len = if key.starts_with("sk-or-") {
        6
    } else if key.starts_with("sk-") {
        3
    } else {
        key.len().min(4)
    };
    let prefix = &key[..prefix_len];
    let suffix = if key.len() >= prefix_len + 4 {
        &key[key.len() - 4..]
    } else {
        ""
    };
    format!("{prefix}••••••••{suffix}")
}

/// Interactive First Start Setup Wizard.
pub struct SetupWizard {
    pub config: AppConfig,
    pub step: usize, // 0: Backend, 1: API Key, 2: Model, 3: Language & Behavior, 4: Sampling & Finish
    pub preset_idx: usize,
    pub custom_url: String,
    pub custom_cursor: usize,
    pub api_key_input: String,
    pub api_key_cursor: usize,
    pub model_idx: usize,
    pub model_search: String,
    pub available_models: Vec<String>,
    pub discovered_models: Vec<flashagent_llm::DiscoveredModel>,
    pub connection_status: Option<String>,
    pub sampling: crate::sampling::SamplingView,
}

impl SetupWizard {
    pub fn new(config: AppConfig) -> Self {
        let presets = BackendPreset::all();
        let is_preset = presets.iter().position(|p| p.url == config.backend_url);
        let (preset_idx, custom_url) = match is_preset {
            Some(idx) => (idx, String::new()),
            None => {
                let url = if config.backend_url.is_empty() {
                    String::new()
                } else {
                    config.backend_url.clone()
                };
                (5, url)
            }
        };
        let custom_cursor = custom_url.len();
        let api_key_input = config.api_key.clone().unwrap_or_default();
        let api_key_cursor = api_key_input.len();
        let sampling = crate::sampling::SamplingView::new(&config);

        Self {
            config,
            step: 0,
            preset_idx,
            custom_url,
            custom_cursor,
            api_key_input,
            api_key_cursor,
            model_idx: 0,
            model_search: String::new(),
            available_models: Vec::new(),
            discovered_models: Vec::new(),
            connection_status: None,
            sampling,
        }
    }

    pub fn total_steps(&self) -> usize {
        5 // 0: Backend, 1: API Key, 2: Model, 3: Language & Behavior, 4: Sampling & Finish
    }

    /// Check if the currently chosen backend URL is a cloud/remote endpoint.
    pub fn is_cloud_backend(&self) -> bool {
        let url = self.config.backend_url.to_lowercase();
        if url.contains("openrouter.ai") {
            return true;
        }
        if url.starts_with("https://") {
            return true;
        }
        if url.contains("localhost") || url.contains("127.0.0.1") || url.contains("0.0.0.0") || url.contains("[::1]") {
            return false;
        }
        true
    }

    /// Check if the selected backend is LM Studio.
    pub fn is_lm_studio(&self) -> bool {
        let url = self.config.backend_url.to_lowercase();
        self.preset_idx == 0 || url.contains("1234") || url.contains("lmstudio")
    }

    /// Store discovered models and filter to loaded models if LM Studio is selected.
    pub fn apply_discovered_models(&mut self, models: Vec<flashagent_llm::DiscoveredModel>) {
        let is_lm = self.is_lm_studio();
        let has_loaded = models.iter().any(|m| m.is_loaded);

        let filtered_models: Vec<flashagent_llm::DiscoveredModel> = if is_lm && has_loaded {
            models.into_iter().filter(|m| m.is_loaded).collect()
        } else {
            models
        };

        self.discovered_models = filtered_models.clone();
        self.available_models = filtered_models.iter().map(|m| m.id.clone()).collect();
        let loaded_count = self.discovered_models.iter().filter(|m| m.is_loaded).count();
        let status = if is_lm && loaded_count > 0 {
            format!("Connected ({loaded_count} loaded model(s) in LM Studio)")
        } else {
            format!("Connected ({} model(s) found)", self.available_models.len())
        };
        self.connection_status = Some(status);
        if let Some(first) = self.available_models.first() {
            self.model_idx = 0;
            self.config.model = first.clone();
        }
    }

    /// Return filtered list of model indices matching `model_search`.
    pub fn filtered_indices(&self) -> Vec<usize> {
        if self.model_search.is_empty() {
            return (0..self.available_models.len()).collect();
        }
        let q = self.model_search.to_lowercase();
        self.available_models
            .iter()
            .enumerate()
            .filter(|(idx, m)| {
                m.to_lowercase().contains(&q)
                    || self
                        .discovered_models
                        .get(*idx)
                        .map(|dm| dm.capabilities_summary().to_lowercase().contains(&q))
                        .unwrap_or(false)
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    fn insert_custom_char(&mut self, c: char) {
        if !self.custom_url.is_char_boundary(self.custom_cursor) {
            self.custom_cursor = self.custom_url.len();
        }
        self.custom_url.insert(self.custom_cursor, c);
        self.custom_cursor += c.len_utf8();
        self.config.backend_url = self.custom_url.clone();
    }

    fn backspace_custom_char(&mut self) {
        if self.custom_cursor > 0 {
            let prev_boundary = self.custom_url[..self.custom_cursor]
                .char_indices()
                .last()
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            self.custom_url.drain(prev_boundary..self.custom_cursor);
            self.custom_cursor = prev_boundary;
            self.config.backend_url = self.custom_url.clone();
        }
    }

    fn delete_custom_char(&mut self) {
        if self.custom_cursor < self.custom_url.len() {
            let next_boundary = self.custom_url[self.custom_cursor..]
                .chars()
                .next()
                .map(|c| self.custom_cursor + c.len_utf8())
                .unwrap_or(self.custom_url.len());
            self.custom_url.drain(self.custom_cursor..next_boundary);
            self.config.backend_url = self.custom_url.clone();
        }
    }

    fn move_cursor_left(&mut self) {
        if self.custom_cursor > 0 {
            self.custom_cursor = self.custom_url[..self.custom_cursor]
                .char_indices()
                .last()
                .map(|(idx, _)| idx)
                .unwrap_or(0);
        }
    }

    fn move_cursor_right(&mut self) {
        if self.custom_cursor < self.custom_url.len() {
            let next_len = self.custom_url[self.custom_cursor..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.custom_cursor += next_len;
        }
    }

    fn sync_api_key(&mut self) {
        let trimmed = self.api_key_input.trim().to_string();
        self.config.api_key = if trimmed.is_empty() { None } else { Some(trimmed) };
    }

    fn insert_api_key_char(&mut self, c: char) {
        if !self.api_key_input.is_char_boundary(self.api_key_cursor) {
            self.api_key_cursor = self.api_key_input.len();
        }
        self.api_key_input.insert(self.api_key_cursor, c);
        self.api_key_cursor += c.len_utf8();
        self.sync_api_key();
    }

    fn backspace_api_key_char(&mut self) {
        if self.api_key_cursor > 0 {
            let prev_boundary = self.api_key_input[..self.api_key_cursor]
                .char_indices()
                .last()
                .map(|(idx, _)| idx)
                .unwrap_or(0);
            self.api_key_input.drain(prev_boundary..self.api_key_cursor);
            self.api_key_cursor = prev_boundary;
            self.sync_api_key();
        }
    }

    fn delete_api_key_char(&mut self) {
        if self.api_key_cursor < self.api_key_input.len() {
            let next_boundary = self.api_key_input[self.api_key_cursor..]
                .chars()
                .next()
                .map(|c| self.api_key_cursor + c.len_utf8())
                .unwrap_or(self.api_key_input.len());
            self.api_key_input.drain(self.api_key_cursor..next_boundary);
            self.sync_api_key();
        }
    }

    /// Handles pasted text into active text fields (Custom backend URL, API key, model search).
    pub fn handle_paste(&mut self, text: &str) {
        for c in text.chars() {
            if c == '\r' || c == '\n' {
                continue;
            }
            match self.step {
                0 if self.preset_idx == 5 => {
                    self.insert_custom_char(c);
                }
                1 => {
                    self.insert_api_key_char(c);
                }
                2 => {
                    self.model_search.push(c);
                    let new_filtered = self.filtered_indices();
                    if !new_filtered.is_empty() && !new_filtered.contains(&self.model_idx) {
                        let chosen_idx = new_filtered[0];
                        self.model_idx = chosen_idx;
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                }
                _ => {}
            }
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<bool> {
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            return Some(false); // cancelled
        }

        // Universal Esc handling across all steps:
        if code == KeyCode::Esc {
            match self.step {
                0 => {
                    if self.preset_idx == 5 && !self.custom_url.is_empty() {
                        self.custom_url.clear();
                        self.custom_cursor = 0;
                        self.config.backend_url.clear();
                        self.connection_status = None;
                        return None;
                    }
                    return Some(false); // abort wizard and exit
                }
                1 => {
                    if !self.api_key_input.is_empty() {
                        self.api_key_input.clear();
                        self.api_key_cursor = 0;
                        self.connection_status = None;
                        return None;
                    }
                    self.step = 0;
                    self.connection_status = None;
                    return None;
                }
                2 => {
                    if !self.model_search.is_empty() {
                        self.model_search.clear();
                        return None;
                    }
                    self.step = 1;
                    return None;
                }
                3 => {
                    self.step = 2;
                    return None;
                }
                4 => {
                    self.step = 3;
                    return None;
                }
                _ => {
                    if self.step > 0 {
                        self.step -= 1;
                        return None;
                    } else {
                        return Some(false);
                    }
                }
            }
        }

        // On step 0 with Custom selected (preset_idx == 5), typing edits the URL directly.
        if self.step == 0 && self.preset_idx == 5 {
            match code {
                KeyCode::Enter => {
                    let mut trimmed = self.custom_url.trim().to_string();
                    if trimmed.is_empty() {
                        self.connection_status = Some("Please enter an endpoint URL before continuing".to_string());
                        return None;
                    }
                    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
                        trimmed = format!("http://{trimmed}");
                    }
                    self.custom_url = trimmed.clone();
                    self.custom_cursor = self.custom_url.len();
                    self.config.backend_url = trimmed;
                    self.connection_status = None;
                    self.step = 1; // Move to API Key Configuration
                    return None;
                }
                KeyCode::Up => {
                    let presets = BackendPreset::all();
                    self.preset_idx = 4;
                    self.config.backend_url = presets[self.preset_idx].url.clone();
                    return None;
                }
                KeyCode::Down => {
                    let presets = BackendPreset::all();
                    self.preset_idx = 0;
                    self.config.backend_url = presets[self.preset_idx].url.clone();
                    return None;
                }
                KeyCode::Left => {
                    self.move_cursor_left();
                    return None;
                }
                KeyCode::Right => {
                    self.move_cursor_right();
                    return None;
                }
                KeyCode::Home => {
                    self.custom_cursor = 0;
                    return None;
                }
                KeyCode::End => {
                    self.custom_cursor = self.custom_url.len();
                    return None;
                }
                KeyCode::Backspace => {
                    self.backspace_custom_char();
                    return None;
                }
                KeyCode::Delete => {
                    self.delete_custom_char();
                    return None;
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                    self.insert_custom_char(c);
                    return None;
                }
                _ => return None,
            }
        }

        // Step 1: API Key Configuration
        if self.step == 1 {
            match code {
                KeyCode::Enter => {
                    let trimmed = self.api_key_input.trim().to_string();
                    if self.is_cloud_backend() && trimmed.is_empty() {
                        self.connection_status = Some("API key is required for cloud providers. Please paste your key.".to_string());
                        return None;
                    }
                    self.config.api_key = if trimmed.is_empty() { None } else { Some(trimmed) };
                    self.connection_status = None;
                    self.step = 2; // Move to Model selection
                    return None;
                }
                KeyCode::Backspace => {
                    self.backspace_api_key_char();
                    return None;
                }
                KeyCode::Delete => {
                    self.delete_api_key_char();
                    return None;
                }
                KeyCode::Left => {
                    if self.api_key_cursor > 0 {
                        self.api_key_cursor = self.api_key_input[..self.api_key_cursor]
                            .char_indices()
                            .last()
                            .map(|(idx, _)| idx)
                            .unwrap_or(0);
                    }
                    return None;
                }
                KeyCode::Right => {
                    if self.api_key_cursor < self.api_key_input.len() {
                        let next_len = self.api_key_input[self.api_key_cursor..]
                            .chars()
                            .next()
                            .map(|c| c.len_utf8())
                            .unwrap_or(0);
                        self.api_key_cursor += next_len;
                    }
                    return None;
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                    self.insert_api_key_char(c);
                    return None;
                }
                KeyCode::Up => {
                    // Navigate back to backend selection
                    self.step = 0;
                    return None;
                }
                _ => return None,
            }
        }

        // Step 2: Model Selection (10 items viewport + live search filter)
        if self.step == 2 {
            let filtered = self.filtered_indices();
            match code {
                KeyCode::Enter => {
                    if !filtered.is_empty() {
                        let sel_pos = filtered.iter().position(|&orig_idx| orig_idx == self.model_idx).unwrap_or(0);
                        let chosen_idx = filtered[sel_pos];
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                    self.step = 3; // Move to Language & Behavior
                    return None;
                }
                KeyCode::Up => {
                    if !filtered.is_empty() {
                        let pos = filtered.iter().position(|&idx| idx == self.model_idx).unwrap_or(0);
                        let new_pos = if pos == 0 { filtered.len() - 1 } else { pos - 1 };
                        let chosen_idx = filtered[new_pos];
                        self.model_idx = chosen_idx;
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                    return None;
                }
                KeyCode::Down => {
                    if !filtered.is_empty() {
                        let pos = filtered.iter().position(|&idx| idx == self.model_idx).unwrap_or(0);
                        let new_pos = (pos + 1) % filtered.len();
                        let chosen_idx = filtered[new_pos];
                        self.model_idx = chosen_idx;
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                    return None;
                }
                KeyCode::PageUp => {
                    if !filtered.is_empty() {
                        let pos = filtered.iter().position(|&idx| idx == self.model_idx).unwrap_or(0);
                        let new_pos = pos.saturating_sub(10);
                        let chosen_idx = filtered[new_pos];
                        self.model_idx = chosen_idx;
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                    return None;
                }
                KeyCode::PageDown => {
                    if !filtered.is_empty() {
                        let pos = filtered.iter().position(|&idx| idx == self.model_idx).unwrap_or(0);
                        let new_pos = (pos + 10).min(filtered.len() - 1);
                        let chosen_idx = filtered[new_pos];
                        self.model_idx = chosen_idx;
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                    return None;
                }
                KeyCode::Backspace => {
                    self.model_search.pop();
                    let new_filtered = self.filtered_indices();
                    if !new_filtered.is_empty() && !new_filtered.contains(&self.model_idx) {
                        let chosen_idx = new_filtered[0];
                        self.model_idx = chosen_idx;
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                    return None;
                }
                KeyCode::Left => {
                    // Navigate back to API key step
                    self.step = 1;
                    return None;
                }
                KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                    self.model_search.push(c);
                    let new_filtered = self.filtered_indices();
                    if !new_filtered.is_empty() && !new_filtered.contains(&self.model_idx) {
                        let chosen_idx = new_filtered[0];
                        self.model_idx = chosen_idx;
                        self.config.model = self.available_models[chosen_idx].clone();
                    }
                    return None;
                }
                _ => return None,
            }
        }

        // Key handling for Step 4 (Sampling & Launch)
        if self.step == 4 {
            let act = self.sampling.handle_key(code, mods);
            match act {
                crate::sampling::SamplingAction::Close => {
                    self.step = 3;
                    return None;
                }
                crate::sampling::SamplingAction::SaveAndClose => {
                    self.sampling.apply_to_config(&mut self.config);
                    self.config.setup_completed = true;
                    let _ = self.config.save();
                    return Some(true);
                }
                crate::sampling::SamplingAction::None => {
                    return None;
                }
            }
        }

        // Key handling for Step 0 (presets), Step 3 (Language & Permissions)
        match code {
            KeyCode::Enter => {
                if self.step + 1 < self.total_steps() {
                    self.step += 1;
                    None
                } else {
                    self.sampling.apply_to_config(&mut self.config);
                    self.config.setup_completed = true;
                    let _ = self.config.save();
                    Some(true) // completed
                }
            }
            KeyCode::Left | KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('h') => {
                self.prev_option();
                None
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('l') => {
                self.next_option();
                None
            }
            KeyCode::Char('1') => {
                self.set_number(1);
                None
            }
            KeyCode::Char('2') => {
                self.set_number(2);
                None
            }
            KeyCode::Char('3') => {
                self.set_number(3);
                None
            }
            KeyCode::Char('4') => {
                self.set_number(4);
                None
            }
            KeyCode::Char('5') => {
                self.set_number(5);
                None
            }
            KeyCode::Char('6') => {
                self.set_number(6);
                None
            }
            _ => None,
        }
    }

    fn next_option(&mut self) {
        let presets = BackendPreset::all();
        match self.step {
            0 => {
                self.preset_idx = (self.preset_idx + 1) % 6;
                if self.preset_idx < presets.len() {
                    self.config.backend_url = presets[self.preset_idx].url.clone();
                } else {
                    self.custom_cursor = self.custom_url.len();
                    self.config.backend_url = self.custom_url.clone();
                }
            }
            3 => {
                // Cycle permission mode: 1. Planning -> 2. Manual -> 3. AcceptEdits -> 4. Bypass -> 1. Planning
                self.config.permission_mode = self.config.permission_mode.next();
            }
            _ => {}
        }
    }

    fn prev_option(&mut self) {
        let presets = BackendPreset::all();
        match self.step {
            0 => {
                self.preset_idx = (self.preset_idx + 6 - 1) % 6;
                if self.preset_idx < presets.len() {
                    self.config.backend_url = presets[self.preset_idx].url.clone();
                } else {
                    self.custom_cursor = self.custom_url.len();
                    self.config.backend_url = self.custom_url.clone();
                }
            }
            3 => {
                self.config.permission_mode = self.config.permission_mode.prev();
            }
            _ => {}
        }
    }

    fn set_number(&mut self, num: usize) {
        let presets = BackendPreset::all();
        if self.step == 0 {
            if num >= 1 && num <= presets.len() {
                self.preset_idx = num - 1;
                self.config.backend_url = presets[self.preset_idx].url.clone();
            } else if num == 6 {
                self.preset_idx = 5;
                self.custom_cursor = self.custom_url.len();
                self.config.backend_url = self.custom_url.clone();
            }
        } else if self.step == 3 {
            match num {
                1 => self.config.permission_mode = PermissionMode::Planning,
                2 => self.config.permission_mode = PermissionMode::Manual,
                3 => self.config.permission_mode = PermissionMode::AcceptEdits,
                4 => self.config.permission_mode = PermissionMode::Bypass,
                _ => {}
            }
        }
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let inner_w = width.saturating_sub(6).clamp(20, 110);

        let title = format!(" FlashAgent First Start Setup ({}/5) ", self.step + 1);
        let dash_count = inner_w.saturating_sub(title.chars().count() + 1);
        lines.push(format!("  {border_color}┌─\x1b[1;38;2;225;175;95m{title}{border_color}{}┐{reset}", "─".repeat(dash_count)));

        let pad_row = |content: &str| -> String {
            let max_w = inner_w.saturating_sub(2);
            let vis = crate::visible_width(content);
            let clipped = if vis > max_w {
                crate::clip_ansi(content, max_w)
            } else {
                content.to_string()
            };
            let clipped_vis = crate::visible_width(&clipped);
            let pad = " ".repeat(max_w.saturating_sub(clipped_vis));
            format!("  {border_color}│{reset} {clipped}{pad} {border_color}│{reset}")
        };

        match self.step {
            0 => {
                lines.push(pad_row("\x1b[1;38;2;240;235;225mStep 1: Choose LLM Backend\x1b[0m"));
                lines.push(pad_row("\x1b[38;2;160;155;145mSelect your local or remote inference engine in 1 click:\x1b[0m"));
                lines.push(pad_row(""));

                let presets = BackendPreset::all();
                for (i, p) in presets.iter().enumerate() {
                    let is_sel = i == self.preset_idx;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let row = if is_sel {
                        format!("{ptr} \x1b[1;38;2;240;235;225m{}. {:<12}\x1b[0m \x1b[38;2;225;175;95m{}\x1b[0m", i + 1, p.name, p.url)
                    } else {
                        format!("{ptr} \x1b[38;2;160;155;145m{}. {:<12}\x1b[0m \x1b[38;2;135;130;125m{}\x1b[0m", i + 1, p.name, p.url)
                    };
                    lines.push(pad_row(&row));
                }

                // 6. Custom backend with keyboard editing & 2x dimmer placeholder
                let is_custom_sel = self.preset_idx == 5;
                let ptr = if is_custom_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                let placeholder_color = "\x1b[38;2;68;65;62m";
                let placeholder_text = "Enter endpoint here";
                let custom_row = if self.custom_url.is_empty() {
                    if is_custom_sel {
                        format!("{ptr} \x1b[1;38;2;240;235;225m6. {:<12}\x1b[0m \x1b[7m \x1b[27m{placeholder_color}{placeholder_text}\x1b[0m", "Custom")
                    } else {
                        format!("{ptr} \x1b[38;2;160;155;145m6. {:<12}\x1b[0m {placeholder_color}{placeholder_text}\x1b[0m", "Custom")
                    }
                } else if is_custom_sel {
                    let (before, at_and_after) = if self.custom_cursor < self.custom_url.len() {
                        let before = &self.custom_url[..self.custom_cursor];
                        let mut rest_chars = self.custom_url[self.custom_cursor..].chars();
                        let cur_char = rest_chars.next().unwrap_or(' ');
                        let after = rest_chars.as_str();
                        (before, format!("\x1b[7m{cur_char}\x1b[27m{after}"))
                    } else {
                        (&self.custom_url[..], "\x1b[7m \x1b[27m".to_string())
                    };
                    format!("{ptr} \x1b[1;38;2;240;235;225m6. {:<12}\x1b[0m \x1b[1;38;2;225;175;95m{before}{at_and_after}\x1b[0m", "Custom")
                } else {
                    format!("{ptr} \x1b[38;2;160;155;145m6. {:<12}\x1b[0m \x1b[38;2;135;130;125m{}\x1b[0m", "Custom", self.custom_url)
                };
                lines.push(pad_row(&custom_row));
                if is_custom_sel {
                    lines.push(pad_row("    \x1b[38;2;175;170;225m(type URL manually · Enter to confirm · ↑/↓ to choose preset)\x1b[0m"));
                }

                if let Some(ref st) = self.connection_status {
                    lines.push(pad_row(""));
                    lines.push(pad_row(&format!("Status: {st}")));
                }
            }
            1 => {
                lines.push(pad_row("\x1b[1;38;2;240;235;225mStep 2: API Key Configuration\x1b[0m"));
                if self.is_cloud_backend() {
                    lines.push(pad_row("\x1b[38;2;225;175;95mCloud / remote inference provider requires an API key.\x1b[0m"));
                    lines.push(pad_row(""));
                    lines.push(pad_row("  \x1b[38;2;160;155;145mHow to get your API key:\x1b[0m"));
                    if self.config.backend_url.contains("openrouter.ai") {
                        lines.push(pad_row("    1. Open \x1b[1;38;2;145;205;140mhttps://openrouter.ai/keys\x1b[0m in your browser"));
                        lines.push(pad_row("    2. Create a key and copy it (format: \x1b[38;2;175;170;225msk-or-v1-...\x1b[0m)"));
                        lines.push(pad_row("    3. Paste or type your key below:"));
                    } else {
                        lines.push(pad_row("    1. Open your provider account / API dashboard"));
                        lines.push(pad_row("    2. Copy your Secret / Bearer API token"));
                        lines.push(pad_row("    3. Paste or type your key below:"));
                    }
                } else {
                    lines.push(pad_row("\x1b[38;2;145;205;140mLocal inference engine detected (zero mandatory cloud keys).\x1b[0m"));
                    lines.push(pad_row(""));
                    lines.push(pad_row("  \x1b[38;2;160;155;145mIf your local server uses Bearer auth, enter it below.\x1b[0m"));
                    lines.push(pad_row("  \x1b[38;2;135;130;125mOtherwise, leave empty and press Enter to continue.\x1b[0m"));
                }
                lines.push(pad_row(""));

                let placeholder_color = "\x1b[38;2;68;65;62m";
                let key_display = if self.api_key_input.is_empty() {
                    let placeholder = if self.is_cloud_backend() {
                        "Paste API key here (sk-or-...)"
                    } else {
                        "Enter API key here (Enter to skip)"
                    };
                    format!("\x1b[7m \x1b[27m{placeholder_color}{placeholder}\x1b[0m")
                } else {
                    let masked = mask_api_key(&self.api_key_input);
                    format!("\x1b[1;38;2;225;175;95m{masked}\x1b[7m \x1b[27m\x1b[0m")
                };
                lines.push(pad_row(&format!("  \x1b[1;38;2;240;235;225mAPI Key:\x1b[0m {key_display}")));

                if let Some(ref st) = self.connection_status {
                    lines.push(pad_row(""));
                    lines.push(pad_row(&format!("  \x1b[38;2;225;110;110m{st}\x1b[0m")));
                }
            }
            2 => {
                let filtered = self.filtered_indices();
                let total_models = self.available_models.len();
                let total_matches = filtered.len();

                lines.push(pad_row("\x1b[1;38;2;240;235;225mStep 3: Select Default Model\x1b[0m"));
                let count_info = if self.is_lm_studio() && self.discovered_models.iter().any(|m| m.is_loaded) {
                    format!("({total_models} loaded in LM Studio)")
                } else if self.model_search.is_empty() {
                    format!("({total_models} models loaded)")
                } else {
                    format!("({total_matches}/{total_models} · filter: \"{}\")", self.model_search)
                };
                lines.push(pad_row(&format!("\x1b[38;2;160;155;145mChoose model \x1b[38;2;175;170;225m{count_info}\x1b[0m:\x1b[0m")));
                lines.push(pad_row(""));

                if self.available_models.is_empty() {
                    lines.push(pad_row(&format!("  \x1b[38;2;225;175;95mAuto-detected model on backend: {}\x1b[0m", if self.config.model.is_empty() { "default" } else { &self.config.model })));
                    lines.push(pad_row("  \x1b[38;2;135;130;125m(Models will be dynamically loaded from server)\x1b[0m"));
                } else if filtered.is_empty() {
                    lines.push(pad_row("  \x1b[38;2;135;130;125m(No models matching search filter)\x1b[0m"));
                } else {
                    let cur_pos = filtered.iter().position(|&idx| idx == self.model_idx).unwrap_or(0);
                    let has_metadata = !self.discovered_models.is_empty();
                    let page_size = if has_metadata { 5 } else { 10 };
                    let start = if cur_pos < page_size { 0 } else { (cur_pos + 1).saturating_sub(page_size) };
                    let end = (start + page_size).min(total_matches);

                    if start > 0 {
                        lines.push(pad_row(&format!("  \x1b[38;2;135;130;125m▲ ... ({} more above) ...\x1b[0m", start)));
                    }

                    for &orig_idx in &filtered[start..end] {
                        let m_raw = &self.available_models[orig_idx];
                        let max_m_len = inner_w.saturating_sub(6);
                        let m = crate::truncate_middle(m_raw, max_m_len);
                        let is_sel = orig_idx == self.model_idx;
                        let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                        let styled = if is_sel {
                            format!("{ptr} \x1b[1;38;2;240;235;225m{m}\x1b[0m")
                        } else {
                            format!("{ptr} \x1b[38;2;160;155;145m{m}\x1b[0m")
                        };
                        lines.push(pad_row(&styled));

                        if let Some(dm) = self.discovered_models.iter().find(|d| d.id == *m_raw) {
                            let summary = dm.capabilities_summary();
                            let load_tag = if dm.is_loaded {
                                "\x1b[1;38;2;145;205;140m● loaded\x1b[0m"
                            } else {
                                "\x1b[38;2;135;130;125m○ available\x1b[0m"
                            };
                            let meta_line = format!("    {load_tag} \x1b[38;2;135;130;125m·\x1b[0m \x1b[38;2;165;175;190m{summary}\x1b[0m");
                            lines.push(pad_row(&meta_line));
                        }
                    }

                    if end < total_matches {
                        lines.push(pad_row(&format!("  \x1b[38;2;135;130;125m▼ ... ({} more below) ...\x1b[0m", total_matches - end)));
                    }
                }
            }
            3 => {
                lines.push(pad_row("\x1b[1;38;2;240;235;225mStep 4: Agent Behavior & Permissions\x1b[0m"));
                lines.push(pad_row("\x1b[38;2;160;155;145mChoose how the agent executes tools and modifies files:\x1b[0m"));
                lines.push(pad_row(""));
                let modes = [
                    (PermissionMode::Planning, "1. Planning", "Require plan review; read-only research"),
                    (PermissionMode::Manual, "2. Manual", "Prompt for confirmation before every file edit or command"),
                    (PermissionMode::AcceptEdits, "3. Accept Edits", "Auto-approve file edits; prompt for commands (Default)"),
                    (PermissionMode::Bypass, "4. Accept All", "Auto-approve all file edits and shell commands"),
                ];
                for (m, label, desc) in modes {
                    let is_sel = self.config.permission_mode == m;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let styled = if is_sel {
                        format!("{ptr} \x1b[1;38;2;240;235;225m{:<16}\x1b[0m \x1b[38;2;225;175;95m{desc}\x1b[0m", label)
                    } else {
                        format!("{ptr} \x1b[38;2;160;155;145m{:<16}\x1b[0m \x1b[38;2;135;130;125m{desc}\x1b[0m", label)
                    };
                    lines.push(pad_row(&styled));
                }
            }
            4 => {
                lines.push(pad_row("\x1b[1;38;2;240;235;225mStep 5: Sampling Parameters & Launch\x1b[0m"));
                lines.push(pad_row("\x1b[38;2;160;155;145mConfigure sampling preset and numeric parameters (direct digit input):\x1b[0m"));
                lines.push(pad_row(""));

                // Preset row
                let is_preset_sel = self.sampling.selected_index == 0;
                let ptr0 = if is_preset_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                let preset_val = format!("[ {} ]", self.sampling.preset.label());
                let (l0, v0) = if is_preset_sel {
                    (
                        format!("\x1b[1;38;2;240;235;225m{:<18}\x1b[0m", "Preset"),
                        format!("\x1b[1;38;2;225;175;95m{:<22}\x1b[0m \x1b[38;2;135;130;125m(←/→ to cycle preset)\x1b[0m", preset_val),
                    )
                } else {
                    (
                        format!("\x1b[38;2;160;155;145m{:<18}\x1b[0m", "Preset"),
                        format!("\x1b[38;2;200;195;185m{:<22}\x1b[0m", preset_val),
                    )
                };
                lines.push(pad_row(&format!("{ptr0} {l0} {v0}")));

                // Parameter rows
                let fields = [
                    (1, "Temperature", &self.sampling.temp_buf, "0.00..2.00 (coding: 0.60, mtp: 0.30, chat: 0.80, precise: 0.10)"),
                    (2, "Top P", &self.sampling.top_p_buf, "0.00..1.00 (coding: 0.95, mtp: 0.90, precise: 0.75)"),
                    (3, "Top K", &self.sampling.top_k_buf, "0..500     (coding: 20, mtp: 40, chat: 50, precise: 10)"),
                    (4, "Repeat Penalty", &self.sampling.repeat_penalty_buf, "0.00..2.00 (coding: 1.00, mtp: 1.02, gemma: 1.08, chat: 1.10)"),
                    (5, "Presence Penalty", &self.sampling.presence_penalty_buf, "-2.00..2.00 (chat: 0.10, mtp-chat: 0.05, default: 0.00)"),
                    (6, "Min P", &self.sampling.min_p_buf, "0.00..1.00 (mtp-code: 0.05, mtp: 0.03, default: 0.00)"),
                ];

                for (idx, label, buf, range_hint) in fields {
                    let is_sel = self.sampling.selected_index == idx;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let display_val = if is_sel { format!("{}█", buf) } else { buf.to_string() };
                    let (label_styled, val_styled) = if is_sel {
                        (
                            format!("\x1b[1;38;2;240;235;225m{:<18}\x1b[0m", label),
                            format!("\x1b[1;38;2;225;175;95m{:<10}\x1b[0m \x1b[38;2;135;130;125m{}\x1b[0m", display_val, range_hint),
                        )
                    } else {
                        (
                            format!("\x1b[38;2;160;155;145m{:<18}\x1b[0m", label),
                            format!("\x1b[38;2;200;195;185m{:<10}\x1b[0m \x1b[38;2;100;95;90m{}\x1b[0m", display_val, range_hint),
                        )
                    };
                    lines.push(pad_row(&format!("{ptr} {label_styled} {val_styled}")));
                }

                // Launch button
                let is_apply_sel = self.sampling.selected_index == 7;
                let ptr7 = if is_apply_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                let apply_styled = if is_apply_sel {
                    "\x1b[1;38;2;145;205;140m[ Save & Launch FlashAgent (Enter) ]\x1b[0m"
                } else {
                    "\x1b[38;2;160;155;145m[ Save & Launch FlashAgent (Enter) ]\x1b[0m"
                };
                lines.push(pad_row(""));
                lines.push(pad_row(&format!("{ptr7} {apply_styled}")));
            }
            _ => {}
        }

        lines.push(pad_row(""));
        match self.step {
            0 => {
                if self.preset_idx == 5 {
                    lines.push(pad_row("\x1b[38;2;135;130;125mtype URL · enter — next · ↑/↓ — presets · esc — clear/quit\x1b[0m"));
                } else {
                    lines.push(pad_row("\x1b[38;2;135;130;125m↑/↓ / 1-6 — select · enter — next · esc — quit\x1b[0m"));
                }
            }
            1 => {
                if self.is_cloud_backend() {
                    lines.push(pad_row("\x1b[38;2;135;130;125mpaste API key · enter — confirm & fetch · esc — clear/back\x1b[0m"));
                } else {
                    lines.push(pad_row("\x1b[38;2;135;130;125mtype key or enter to skip · esc — clear/back\x1b[0m"));
                }
            }
            2 => {
                lines.push(pad_row("\x1b[38;2;135;130;125m↑/↓ · PgUp/PgDn · type to search · enter — select · esc — clear/back\x1b[0m"));
            }
            3 => {
                lines.push(pad_row("\x1b[38;2;135;130;125m↑/↓ / 1-4 — select mode · enter — next · esc — back\x1b[0m"));
            }
            4 => {
                lines.push(pad_row("\x1b[38;2;135;130;125mtype digits/./- · ←/→ adjust · ↑/↓ navigate · Enter launch · Esc back\x1b[0m"));
            }
            _ => {}
        }

        lines.push(format!("  {border_color}└{}┘{reset}", "─".repeat(inner_w)));
        lines
    }
}

/// Run interactive setup wizard in alternate screen using an active event channel.
/// Eliminates stdin contention with the main event reader thread during in-app execution.
pub async fn run_wizard_channel(
    config: &mut AppConfig,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::UiEvent>,
) -> anyhow::Result<bool> {
    use crossterm::{
        cursor,
        execute,
        terminal::{EnterAlternateScreen, LeaveAlternateScreen},
    };

    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, cursor::Hide);

    let mut wizard = SetupWizard::new(config.clone());

    // Try probing default backend for models
    let backend = flashagent_llm::OpenAiCompat::new(&wizard.config.backend_url, "", wizard.config.api_key.clone());
    if let Some(disc) = backend.discover_server().await {
        wizard.apply_discovered_models(disc.models);
    }

    let completed = loop {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let lines = wizard.render(term_w as usize);

        let mut buffer = String::from("\x1b[H\x1b[2J\r\n");
        for line in lines {
            buffer.push_str(&line);
            buffer.push_str("\r\n");
        }
        let _ = stdout.write_all(buffer.as_bytes());
        let _ = stdout.flush();

        tokio::select! {
            Some(ev) = rx.recv() => {
                match ev {
                    crate::UiEvent::Key(code, mods) => {
                        let prev_step = wizard.step;
                        if let Some(finished) = wizard.handle_key(code, mods) {
                            break finished;
                        }
                        // Step transition probe: Step 1 (API Key) to Step 2 (Model selection)
                        if prev_step == 1 && wizard.step == 2 {
                            let backend = flashagent_llm::OpenAiCompat::new(&wizard.config.backend_url, "", wizard.config.api_key.clone());
                            if let Some(disc) = backend.discover_server().await {
                                wizard.apply_discovered_models(disc.models);
                            } else {
                                wizard.available_models.clear();
                                wizard.discovered_models.clear();
                                wizard.connection_status = Some("Could not reach backend (will use default)".to_string());
                            }
                        }
                    }
                    crate::UiEvent::Paste(text) => {
                        wizard.handle_paste(&text);
                    }
                    crate::UiEvent::Resize(_, _) => {}
                    _ => {}
                }
            }
        }
    };

    if completed {
        *config = wizard.config;
    }

    let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
    // Raw mode is intentionally preserved since run_app remains active
    Ok(completed)
}

/// Run interactive setup wizard in alternate screen (for standalone startup flow).
pub async fn run_wizard(config: &mut AppConfig) -> anyhow::Result<bool> {
    use crossterm::{
        cursor,
        execute,
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    };

    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, cursor::Hide);

    let mut wizard = SetupWizard::new(config.clone());

    // Try probing default backend for models
    let backend = flashagent_llm::OpenAiCompat::new(&wizard.config.backend_url, "", wizard.config.api_key.clone());
    if let Some(disc) = backend.discover_server().await {
        wizard.apply_discovered_models(disc.models);
    }

    let completed = loop {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let lines = wizard.render(term_w as usize);

        let mut buffer = String::from("\x1b[H\x1b[2J\r\n");
        for line in lines {
            buffer.push_str(&line);
            buffer.push_str("\r\n");
        }
        let _ = stdout.write_all(buffer.as_bytes());
        let _ = stdout.flush();

        if crossterm::event::poll(std::time::Duration::from_millis(50))? {
            match crossterm::event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let prev_step = wizard.step;
                    if let Some(finished) = wizard.handle_key(key.code, key.modifiers) {
                        break finished;
                    }
                    // When transitioning from Step 1 (API Key) to Step 2 (Model selection), probe server with key
                    if prev_step == 1 && wizard.step == 2 {
                        let backend = flashagent_llm::OpenAiCompat::new(&wizard.config.backend_url, "", wizard.config.api_key.clone());
                        if let Some(disc) = backend.discover_server().await {
                            wizard.apply_discovered_models(disc.models);
                        } else {
                            wizard.available_models.clear();
                            wizard.discovered_models.clear();
                            wizard.connection_status = Some("Could not reach backend (will use default)".to_string());
                        }
                    }
                }
                Event::Paste(text) => {
                    wizard.handle_paste(&text);
                }
                _ => {}
            }
        }
    };

    if completed {
        *config = wizard.config;
    }

    let _ = execute!(stdout, cursor::Show, LeaveAlternateScreen);
    disable_raw_mode()?;

    Ok(completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wizard_steps_and_navigation() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);
        assert_eq!(wizard.step, 0);

        wizard.next_option();
        assert_eq!(wizard.preset_idx, 1);

        // Step 0 -> Step 1 (API Key)
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);

        // Step 1 -> Step 2 (Model) - local backend allows skipping empty key
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 2);

        // Step 2 -> Step 3 (Agent Behavior)
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 3);
        assert_eq!(wizard.config.permission_mode, PermissionMode::AcceptEdits);
        wizard.set_number(1);
        assert_eq!(wizard.config.permission_mode, PermissionMode::Planning);
        wizard.set_number(3);
        assert_eq!(wizard.config.permission_mode, PermissionMode::AcceptEdits);

        // Step 3 -> Step 4 (Sampling)
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 4);

        // Test Esc navigation backwards:
        wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(wizard.step, 3);

        wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(wizard.step, 2);

        wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);

        wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(wizard.step, 0);

        // On Step 0, Esc exits (returns Some(false))
        let res = wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(res, Some(false));
    }

    #[test]
    fn test_wizard_cloud_mandatory_api_key() {
        let cfg = AppConfig { backend_url: "https://openrouter.ai/api/v1".to_string(), ..AppConfig::default() };
        let mut wizard = SetupWizard::new(cfg);
        wizard.step = 1; // On API key step

        assert!(wizard.is_cloud_backend());

        // Pressing Enter on empty key must fail and stay on step 1
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);
        assert!(wizard.connection_status.as_ref().unwrap().contains("API key is required"));

        // Type key: sk-or-v1-abcdef123456
        for c in "sk-or-v1-abcdef123456".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        assert_eq!(wizard.api_key_input, "sk-or-v1-abcdef123456");

        // Verify masked render
        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("sk-or-••••••••3456"));

        // Now Enter succeeds and moves to step 2
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 2);
        assert_eq!(wizard.config.api_key.as_deref(), Some("sk-or-v1-abcdef123456"));
    }

    #[test]
    fn test_wizard_model_scrolling_and_live_search() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);
        wizard.step = 2;
        // Populate 430 models
        wizard.available_models = (0..430)
            .map(|i| format!("provider/model-variant-{i:03}"))
        .collect();

        // 10 items shown in window
        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("430 models loaded"));
        assert!(rendered.contains("▼ ... (420 more below)"));

        // Type live search "variant-04"
        for c in "variant-04".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        assert_eq!(wizard.model_search, "variant-04");
        let search_rendered = wizard.render(80).join("\n");
        assert!(search_rendered.contains("filter: \"variant-04\""));

        // Page down
        wizard.handle_key(KeyCode::PageDown, KeyModifiers::empty());

        // Backspace search
        while !wizard.model_search.is_empty() {
            wizard.handle_key(KeyCode::Backspace, KeyModifiers::empty());
        }
        assert_eq!(wizard.model_search, "");
    }

    #[test]
    fn test_wizard_custom_backend_selection_and_typing() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);

        // Custom field is initially empty and shows placeholder "Enter endpoint here"
        assert_eq!(wizard.custom_url, "");
        let initial_render = wizard.render(80).join("\n");
        assert!(initial_render.contains("Enter endpoint here"));

        // Select '6' for Custom
        wizard.handle_key(KeyCode::Char('6'), KeyModifiers::empty());
        assert_eq!(wizard.preset_idx, 5);
        let selected_render = wizard.render(80).join("\n");
        assert!(selected_render.contains("Enter endpoint here"));
        assert!(selected_render.contains("type URL manually"));

        // Enter on empty input does NOT advance and shows warning
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 0);
        assert!(wizard.connection_status.as_ref().unwrap().contains("Please enter an endpoint URL"));

        // Type first character 'h' -> placeholder disappears
        wizard.handle_key(KeyCode::Char('h'), KeyModifiers::empty());
        assert_eq!(wizard.custom_url, "h");
        let typed_render = wizard.render(80).join("\n");
        assert!(!typed_render.contains("Enter endpoint here"));
        assert!(typed_render.contains("h"));

        // Type the rest of "ttp://192.168.1.50:5000/v1"
        for c in "ttp://192.168.1.50:5000/v1".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        assert_eq!(wizard.custom_url, "http://192.168.1.50:5000/v1");
        assert_eq!(wizard.config.backend_url, "http://192.168.1.50:5000/v1");

        // Backspace everything -> placeholder reappears
        while !wizard.custom_url.is_empty() {
            wizard.handle_key(KeyCode::Backspace, KeyModifiers::empty());
        }
        assert_eq!(wizard.custom_url, "");
        let emptied_render = wizard.render(80).join("\n");
        assert!(emptied_render.contains("Enter endpoint here"));

        // Re-type URL and confirm with Enter -> moves to Step 1 (API key)
        for c in "http://192.168.1.50:5000/v1".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);
        assert_eq!(wizard.config.backend_url, "http://192.168.1.50:5000/v1");
    }

    #[test]
    fn test_wizard_custom_navigation_and_arrows() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);

        // Jump to Custom (index 5)
        wizard.handle_key(KeyCode::Char('6'), KeyModifiers::empty());
        assert_eq!(wizard.preset_idx, 5);

        // Up arrow navigates back to OpenRouter (preset 4)
        wizard.handle_key(KeyCode::Up, KeyModifiers::empty());
        assert_eq!(wizard.preset_idx, 4);
        assert_eq!(wizard.config.backend_url, "https://openrouter.ai/api/v1");

        // Down arrow navigates to Custom (preset 5)
        wizard.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(wizard.preset_idx, 5);
    }

    #[test]
    fn test_wizard_custom_auto_prefix_http() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);

        // Select Custom
        wizard.handle_key(KeyCode::Char('6'), KeyModifiers::empty());

        // Type "localhost:8000/v1" without http://
        for c in "localhost:8000/v1".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }

        // Press Enter moves to step 1
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);
        assert_eq!(wizard.config.backend_url, "http://localhost:8000/v1");
    }

    #[test]
    fn test_wizard_preserves_existing_custom_url() {
        let cfg = AppConfig { backend_url: "http://10.0.0.5:1234/v1".to_string(), ..AppConfig::default() };

        let wizard = SetupWizard::new(cfg);
        assert_eq!(wizard.preset_idx, 5);
        assert_eq!(wizard.custom_url, "http://10.0.0.5:1234/v1");
        assert_eq!(wizard.custom_cursor, "http://10.0.0.5:1234/v1".len());
        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("http://10.0.0.5:1234/v1"));
        assert!(!rendered.contains("Enter endpoint here"));
    }

    #[test]
    fn test_wizard_lm_studio_loaded_models_filtering_and_capabilities() {
        let cfg = AppConfig { backend_url: "http://127.0.0.1:1234/v1".to_string(), ..AppConfig::default() };

        let mut wizard = SetupWizard::new(cfg);
        assert!(wizard.is_lm_studio());

        let models = vec![
            flashagent_llm::DiscoveredModel {
                id: "qwen2.5-7b-instruct".to_string(),
                display_name: None,
                is_loaded: false,
                context_length: Some(32768),
                max_context_length: Some(32768),
                thinking: flashagent_llm::ThinkingProfile::unsupported(),
                supports_tools: true,
                supports_vision: false,
            },
            flashagent_llm::DiscoveredModel {
                id: "deepseek-r1-distill-qwen-14b".to_string(),
                display_name: None,
                is_loaded: true,
                context_length: Some(131072),
                max_context_length: Some(131072),
                thinking: flashagent_llm::ThinkingProfile {
                    presets: vec!["off".to_string(), "on".to_string()],
                    protocol: flashagent_llm::ThinkingProtocol::LmStudio,
                    supported: true,
                    default_preset: Some("on".to_string()),
                },
                supports_tools: true,
                supports_vision: false,
            },
            flashagent_llm::DiscoveredModel {
                id: "llama-3.2-3b-instruct".to_string(),
                display_name: None,
                is_loaded: false,
                context_length: Some(8192),
                max_context_length: Some(8192),
                thinking: flashagent_llm::ThinkingProfile::unsupported(),
                supports_tools: false,
                supports_vision: true,
            },
        ];

        wizard.apply_discovered_models(models);

        // Only 1 loaded model should be kept
        assert_eq!(wizard.available_models.len(), 1);
        assert_eq!(wizard.available_models[0], "deepseek-r1-distill-qwen-14b");
        assert_eq!(wizard.config.model, "deepseek-r1-distill-qwen-14b");

        wizard.step = 2;
        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("(1 loaded in LM Studio)"));
        assert!(rendered.contains("deepseek-r1-distill-qwen-14b"));
        assert!(rendered.contains("128k ctx · tools · thinking: on [off,on]"));
    }

    #[test]
    fn test_wizard_handle_paste_instantly_updates_fields() {
        let mut wizard = SetupWizard::new(AppConfig::default());
        wizard.preset_idx = 5; // Custom backend

        // Paste custom URL
        wizard.handle_paste("http://192.168.1.50:8000/v1\r\n");
        assert_eq!(wizard.custom_url, "http://192.168.1.50:8000/v1");
        assert_eq!(wizard.config.backend_url, "http://192.168.1.50:8000/v1");

        // Advance to step 1 (API key)
        wizard.step = 1;
        wizard.handle_paste("sk-paste-token-xyz");
        assert_eq!(wizard.api_key_input, "sk-paste-token-xyz");
        assert_eq!(wizard.config.api_key.as_deref(), Some("sk-paste-token-xyz"));

        // Advance to step 2 (Model search)
        wizard.step = 2;
        wizard.available_models = vec![
            "llama-3.1-8b-instruct".to_string(),
            "deepseek-coder-v2".to_string(),
            "qwen2.5-coder-7b".to_string(),
        ];
        wizard.handle_paste("coder");
        assert_eq!(wizard.model_search, "coder");
        assert_eq!(wizard.config.model, "deepseek-coder-v2");
    }
}
