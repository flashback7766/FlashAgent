//! First-run setup wizard: provider, API key, model, behaviour, sampling. Run
//! again, it adds a provider or updates the one for the same server.

use flashagent_core::config::{KeySource, ProviderProfile};
use flashagent_core::{AppConfig, BackendPreset, PermissionMode};
use flashagent_llm::ApiProtocol;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

/// First characters and the last 4, when long enough.
pub fn mask_api_key(key: &str) -> String {
    if key.is_empty() {
        return String::new();
    }
    // By characters: a key typed on a Russian layout is not ASCII.
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 6 {
        return "•".repeat(chars.len());
    }
    let prefix_len = if key.starts_with("sk-or-") {
        6
    } else if key.starts_with("sk-") {
        3
    } else {
        4
    };
    let prefix: String = chars[..prefix_len].iter().collect();
    let suffix: String = if chars.len() >= prefix_len + 4 { chars[chars.len() - 4..].iter().collect() } else { String::new() };
    format!("{prefix}••••••••{suffix}")
}

/// Preset rows shown at once; the list scrolls past them.
const PRESET_WINDOW: usize = 10;

pub struct SetupWizard {
    pub config: AppConfig,
    /// The provider being set up. Saved into `config` when the wizard ends,
    /// over the one for the same server if there is one.
    pub profile: ProviderProfile,
    pub step: usize, // 0: Provider, 1: API Key, 2: Model, 3: Permissions, 4: Sampling & Finish
    /// Only the provider's steps: permissions and sampling are left as they are.
    pub provider_only: bool,
    pub preset_idx: usize,
    pub custom_url: String,
    pub custom_cursor: usize,
    /// Tab on the custom row; `None` reads it from the address.
    pub custom_protocol: Option<ApiProtocol>,
    pub api_key_input: String,
    pub api_key_cursor: usize,
    pub model_idx: usize,
    pub model_search: String,
    pub available_models: Vec<String>,
    pub discovered_models: Vec<flashagent_llm::DiscoveredModel>,
    pub connection_status: Option<String>,
    pub sampling: crate::sampling::SamplingView,
    /// `None` until discovery has run, which is not "not LM Studio". Once it
    /// answers, its word beats the URL guess in `is_lm_studio`, even for "Other".
    discovered_kind: Option<flashagent_llm::thinking::ServerKind>,
}

impl SetupWizard {
    /// Opens on the provider in use, its key and model filled in.
    pub fn new(config: AppConfig) -> Self {
        let profile = config.active_profile().clone();
        let presets = BackendPreset::all();
        let preset_row = presets.iter().position(|p| ProviderProfile::from_preset(p).same_server(&profile));
        let (preset_idx, custom_url, custom_protocol) = match preset_row {
            Some(idx) => (idx, String::new(), None),
            None => {
                let protocol = (profile.protocol != ApiProtocol::detect(&profile.url)).then_some(profile.protocol);
                (Self::custom_idx(), profile.url.clone(), protocol)
            }
        };
        let custom_cursor = custom_url.len();
        let api_key_input = profile.api_key.clone().unwrap_or_default();
        let api_key_cursor = api_key_input.len();
        let sampling = crate::sampling::SamplingView::new(&config);

        Self {
            config,
            profile,
            step: 0,
            provider_only: false,
            preset_idx,
            custom_url,
            custom_cursor,
            custom_protocol,
            api_key_input,
            api_key_cursor,
            model_idx: 0,
            model_search: String::new(),
            available_models: Vec::new(),
            discovered_models: Vec::new(),
            connection_status: None,
            sampling,
            discovered_kind: None,
        }
    }

    /// Another provider beside the saved ones: server, key and model only.
    pub fn adding(config: AppConfig) -> Self {
        let mut wizard = Self::new(config);
        wizard.provider_only = true;
        wizard.custom_url.clear();
        wizard.custom_protocol = None;
        wizard.select_row(0);
        wizard
    }

    /// Custom is the row after the presets, however many there are.
    fn custom_idx() -> usize {
        BackendPreset::all().len()
    }

    fn is_custom(&self) -> bool {
        self.preset_idx == Self::custom_idx()
    }

    fn preset(&self) -> Option<&'static BackendPreset> {
        BackendPreset::all().get(self.preset_idx)
    }

    /// The key that picks a row: 1–9, then 0, and c for Custom.
    fn row_key(i: usize) -> Option<char> {
        match i {
            _ if i == Self::custom_idx() => Some('c'),
            0..=8 => char::from_digit(i as u32 + 1, 10),
            9 => Some('0'),
            _ => None,
        }
    }

    /// Where the typed address goes, and how it is spoken to.
    fn custom_profile(&self) -> ProviderProfile {
        let mut profile = ProviderProfile::from_url(&self.custom_url);
        if let Some(protocol) = self.custom_protocol {
            profile.protocol = protocol;
        }
        profile
    }

    /// The provider a row stands for: the one saved for that server, with its
    /// key and model, or a new one.
    fn select_row(&mut self, row: usize) {
        self.preset_idx = row.min(Self::custom_idx());
        let fresh = match self.preset() {
            Some(p) => ProviderProfile::from_preset(p),
            None => {
                self.custom_cursor = self.custom_url.len();
                self.custom_profile()
            }
        };
        self.profile = self.config.providers.iter().find(|p| p.same_server(&fresh)).cloned().unwrap_or(fresh);
        self.api_key_input = self.profile.api_key.clone().unwrap_or_default();
        self.api_key_cursor = self.api_key_input.len();
    }

    /// An address typed to a saved server brings that provider's key; typed
    /// on past it, the key is dropped rather than sent to another server.
    fn sync_custom(&mut self) {
        let typed = self.custom_profile();
        match self.config.providers.iter().find(|p| p.same_server(&typed)).cloned() {
            Some(saved) => {
                self.api_key_input = saved.api_key.clone().unwrap_or_default();
                self.api_key_cursor = self.api_key_input.len();
                self.profile = saved;
            }
            None => {
                if self.is_saved(&self.profile) {
                    self.api_key_input.clear();
                    self.api_key_cursor = 0;
                }
                self.profile = typed;
                self.sync_api_key();
            }
        }
    }

    /// The steps this run shows, in order: no key for a server on this
    /// computer, no permissions or sampling when only adding a provider.
    fn shown_steps(&self) -> Vec<usize> {
        let mut steps = vec![0];
        if self.needs_key_step() {
            steps.push(1);
        }
        steps.push(2);
        if !self.provider_only {
            steps.extend([3, 4]);
        }
        steps
    }

    /// 1-based, as shown.
    fn step_number(&self, step: usize) -> usize {
        let steps = self.shown_steps();
        steps.iter().position(|s| *s == step).map_or(step + 1, |i| i + 1)
    }

    pub fn total_steps(&self) -> usize {
        self.shown_steps().len()
    }

    /// A preset for a server on this computer needs no key and skips the
    /// step; an address typed by hand may still want one.
    pub fn needs_key_step(&self) -> bool {
        self.preset().is_none_or(|p| !p.is_local())
    }

    /// A cloud preset, or a typed address reached over https: a cloud API is
    /// never plain http, and a server on this computer or network rarely https.
    pub fn is_cloud_backend(&self) -> bool {
        match self.preset() {
            Some(p) => !p.is_local(),
            None => self.profile.url.to_lowercase().starts_with("https://"),
        }
    }

    /// Only a cloud preset cannot go on without a key: a typed address may be a
    /// server that takes none, wherever it is.
    fn key_required(&self) -> bool {
        self.preset().is_some_and(|p| !p.is_local())
    }

    /// The environment variable a key would be read from if none is typed.
    pub fn env_key(&self) -> Option<&'static str> {
        let mut keyless = self.profile.clone();
        keyless.api_key = None;
        match keyless.key_source() {
            KeySource::Env(var) => Some(var),
            _ => None,
        }
    }

    /// A cloud provider with no key typed and none in the environment.
    fn key_missing(&self) -> bool {
        self.key_required() && self.api_key_input.trim().is_empty() && self.env_key().is_none()
    }

    /// The server's own answer once discovery has run; before that, a guess from
    /// the URL for choosing defaults.
    pub fn is_lm_studio(&self) -> bool {
        if let Some(kind) = self.discovered_kind {
            return kind == flashagent_llm::thinking::ServerKind::LmStudio;
        }
        let url = self.profile.url.to_lowercase();
        self.preset_idx == 0 || url.contains("1234") || url.contains("lmstudio")
    }

    /// Keeps only loaded models when the server is LM Studio. The model saved
    /// for this provider stays chosen when the server has it.
    pub fn apply_discovered_models(&mut self, disc: flashagent_llm::ServerDiscovery) {
        self.discovered_kind = Some(disc.kind);
        let models = disc.models;
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
            format!("Connected ({loaded_count} loaded {} in LM Studio)", if loaded_count == 1 { "model" } else { "models" })
        } else {
            let n = self.available_models.len();
            format!("Connected ({n} {} found)", if n == 1 { "model" } else { "models" })
        };
        self.connection_status = Some(status);
        let saved = self.available_models.iter().position(|m| *m == self.profile.model);
        if let Some(idx) = saved.or((!self.available_models.is_empty()).then_some(0)) {
            self.model_idx = idx;
            self.profile.model = self.available_models[idx].clone();
        }
    }

    /// Matching `model_search`.
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
        self.sync_custom();
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
            self.sync_custom();
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
            self.sync_custom();
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
        self.profile.api_key = if trimmed.is_empty() { None } else { Some(trimmed) };
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

    /// Into the active text field: custom URL, API key or model search.
    pub fn handle_paste(&mut self, text: &str) {
        for c in text.chars() {
            // A key copied from a web page often brings a space or line break with it.
            if c == '\r' || c == '\n' || (self.step == 1 && c.is_whitespace()) {
                continue;
            }
            match self.step {
                0 if self.is_custom() => {
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
                        self.profile.model = self.available_models[chosen_idx].clone();
                    }
                }
                _ => {}
            }
        }
    }

    /// Past the provider's row: to the key, or straight to its models.
    fn leave_provider_step(&mut self) {
        self.connection_status = None;
        self.step = if self.needs_key_step() { 1 } else { 2 };
    }

    /// The setup is saved: the provider over the one for its server or beside
    /// the others, and made the one in use.
    fn finish(&mut self) -> Option<bool> {
        self.config.save_provider(self.profile.clone());
        if !self.provider_only {
            self.sampling.apply_to_config(&mut self.config);
            self.config.setup_completed = true;
        }
        let _ = self.config.save();
        Some(true)
    }

    fn choose_model(&mut self, idx: usize) {
        self.model_idx = idx;
        self.profile.model = self.available_models[idx].clone();
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<bool> {
        if code == KeyCode::Char('c') && mods.contains(KeyModifiers::CONTROL) {
            return Some(false); // cancelled
        }

        // Ctrl+V (Ctrl+М on a Russian layout) and Shift+Insert read the clipboard
        // here: the classic Windows console hands them over as keys instead of
        // pasting, so the API key could not be pasted at all.
        let paste_key = (mods.contains(KeyModifiers::CONTROL)
            && matches!(code, KeyCode::Char('v' | 'V' | '\u{043c}' | '\u{041c}')))
            || (code == KeyCode::Insert && mods.contains(KeyModifiers::SHIFT));
        if paste_key {
            let takes_text = matches!(self.step, 1 | 2) || (self.step == 0 && self.is_custom());
            if takes_text {
                match crate::clipboard::get_clipboard_text() {
                    Some(text) => self.handle_paste(&text),
                    None => self.connection_status = Some("The clipboard has no text to paste".to_string()),
                }
            }
            return None;
        }

        if code == KeyCode::Esc {
            match self.step {
                0 => {
                    if self.is_custom() && !self.custom_url.is_empty() {
                        self.custom_url.clear();
                        self.custom_cursor = 0;
                        self.sync_custom();
                        self.connection_status = None;
                        return None;
                    }
                    return Some(false); // abort wizard and exit
                }
                1 => {
                    if !self.api_key_input.is_empty() {
                        self.api_key_input.clear();
                        self.api_key_cursor = 0;
                        self.sync_api_key();
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
                    self.step = if self.needs_key_step() { 1 } else { 0 };
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

        // Step 0 with Custom selected: typing edits the URL, Tab the protocol.
        if self.step == 0 && self.is_custom() {
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
                    self.custom_url = trimmed;
                    self.custom_cursor = self.custom_url.len();
                    self.sync_custom();
                    self.leave_provider_step();
                    return None;
                }
                KeyCode::Tab | KeyCode::BackTab => {
                    let all = ApiProtocol::ALL;
                    let current = all.iter().position(|p| *p == self.custom_profile().protocol).unwrap_or(0);
                    let next = if code == KeyCode::Tab { (current + 1) % all.len() } else { (current + all.len() - 1) % all.len() };
                    self.custom_protocol = Some(all[next]);
                    self.sync_custom();
                    return None;
                }
                KeyCode::Up => {
                    self.select_row(Self::custom_idx() - 1);
                    return None;
                }
                KeyCode::Down => {
                    self.select_row(0);
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

        // Step 1: API key.
        if self.step == 1 {
            match code {
                KeyCode::Enter => {
                    if self.key_missing() {
                        self.connection_status = Some("API key is required for cloud providers. Please paste your key.".to_string());
                        return None;
                    }
                    self.sync_api_key();
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
                    self.step = 0;
                    return None;
                }
                _ => return None,
            }
        }

        // Step 2: model, a 10-item window with live search.
        if self.step == 2 {
            let filtered = self.filtered_indices();
            let pos = filtered.iter().position(|&idx| idx == self.model_idx).unwrap_or(0);
            match code {
                KeyCode::Enter => {
                    if !filtered.is_empty() {
                        self.choose_model(filtered[pos]);
                    }
                    if self.provider_only {
                        return self.finish();
                    }
                    self.step = 3; // Move to Permissions
                    return None;
                }
                KeyCode::Up if !filtered.is_empty() => {
                    self.choose_model(filtered[if pos == 0 { filtered.len() - 1 } else { pos - 1 }]);
                }
                KeyCode::Down if !filtered.is_empty() => self.choose_model(filtered[(pos + 1) % filtered.len()]),
                KeyCode::PageUp if !filtered.is_empty() => self.choose_model(filtered[pos.saturating_sub(10)]),
                KeyCode::PageDown if !filtered.is_empty() => self.choose_model(filtered[(pos + 10).min(filtered.len() - 1)]),
                KeyCode::Backspace | KeyCode::Char(_) => {
                    match code {
                        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                            self.model_search.push(c)
                        }
                        KeyCode::Backspace => {
                            self.model_search.pop();
                        }
                        _ => return None,
                    }
                    let new_filtered = self.filtered_indices();
                    if !new_filtered.is_empty() && !new_filtered.contains(&self.model_idx) {
                        self.choose_model(new_filtered[0]);
                    }
                }
                KeyCode::Left => self.step = if self.needs_key_step() { 1 } else { 0 },
                _ => {}
            }
            return None;
        }

        // Step 4: a sampling preset and launch. The numbers behind a preset are for
        // those who look for them, in Settings → Reasoning → Sampling (advanced).
        if self.step == 4 {
            match code {
                KeyCode::Esc => self.step = 3,
                KeyCode::Enter => return self.finish(),
                KeyCode::Left | KeyCode::Right => {
                    self.sampling.selected_index = 0;
                    let _ = self.sampling.handle_key(code, mods);
                }
                _ => {}
            }
            return None;
        }

        // Step 0 (presets) and step 3 (behaviour).
        match code {
            KeyCode::Enter => {
                match self.step {
                    0 => self.leave_provider_step(),
                    3 => self.step = 4,
                    _ => {}
                }
                None
            }
            KeyCode::Left | KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('h') => {
                self.prev_option();
                None
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('l') => {
                self.next_option();
                None
            }
            KeyCode::Char(c) => {
                self.pick_key(c);
                None
            }
            _ => None,
        }
    }

    fn next_option(&mut self) {
        match self.step {
            0 => self.select_row((self.preset_idx + 1) % (Self::custom_idx() + 1)),
            3 => self.config.permission_mode = self.config.permission_mode.next(),
            _ => {}
        }
    }

    fn prev_option(&mut self) {
        match self.step {
            0 => self.select_row((self.preset_idx + Self::custom_idx()) % (Self::custom_idx() + 1)),
            3 => self.config.permission_mode = self.config.permission_mode.prev(),
            _ => {}
        }
    }

    fn pick_key(&mut self, key: char) {
        if self.step == 0 {
            if let Some(row) = (0..=Self::custom_idx()).find(|&i| Self::row_key(i) == Some(key.to_ascii_lowercase())) {
                self.select_row(row);
            }
        } else if self.step == 3 {
            match key {
                '1' => self.config.permission_mode = PermissionMode::Planning,
                '2' => self.config.permission_mode = PermissionMode::Manual,
                '3' => self.config.permission_mode = PermissionMode::AcceptEdits,
                '4' => self.config.permission_mode = PermissionMode::Bypass,
                _ => {}
            }
        }
    }

    /// A row stands for a provider already saved for that server.
    fn is_saved(&self, profile: &ProviderProfile) -> bool {
        self.config.providers.iter().any(|p| p.same_server(profile))
    }

    pub fn render(&self, width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";
        let inner_w = width.saturating_sub(6).clamp(20, 110);

        let what = if self.provider_only { "Add a provider" } else { "FlashAgent setup" };
        let title = format!(" {what} \u{b7} step {} of {} ", self.step_number(self.step), self.total_steps());
        let dash_count = inner_w.saturating_sub(title.chars().count() + 1);
        lines.push(format!("  {border_color}╭─\x1b[1;38;2;225;175;95m{title}{border_color}{}╮{reset}", "─".repeat(dash_count)));

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
        let heading = |step: usize, text: &str| format!("\x1b[1;38;2;240;235;225mStep {}: {text}\x1b[0m", self.step_number(step));

        match self.step {
            0 => {
                lines.push(pad_row(&heading(0, "Choose your model server")));
                lines.push(pad_row("\x1b[38;2;160;155;145mWhere does your model run? A local server needs no key.\x1b[0m"));
                lines.push(pad_row(""));

                let presets = BackendPreset::all();
                let rows = presets.len() + 1;
                let start = (self.preset_idx + 1).saturating_sub(PRESET_WINDOW).min(rows.saturating_sub(PRESET_WINDOW + 1));
                let end = (start + PRESET_WINDOW).min(presets.len());
                if start > 0 {
                    lines.push(pad_row(&format!("  \x1b[38;2;135;130;125m▲ {start} more above\x1b[0m")));
                }
                // Names in one column when the window has room for the longest; the
                // address gives way, from its middle, to the "saved" mark.
                let longest = presets.iter().map(|p| p.name.chars().count()).max().unwrap_or(12);
                let name_w = if inner_w >= 60 { longest } else { 12 };
                for (i, p) in presets.iter().enumerate().take(end).skip(start) {
                    let is_sel = i == self.preset_idx;
                    let ptr = if is_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                    let key = Self::row_key(i).map_or_else(|| "  ".to_string(), |k| format!("{k}."));
                    let is_saved = self.is_saved(&ProviderProfile::from_preset(p));
                    let saved = if is_saved { "  \x1b[38;2;135;130;125msaved\x1b[0m" } else { "" };
                    let url_room = inner_w
                        .saturating_sub(2 + 5 + name_w.max(p.name.chars().count()) + 1 + if is_saved { 7 } else { 0 })
                        .max(12);
                    let url = crate::truncate_middle(p.url, url_room);
                    let row = if is_sel {
                        format!("{ptr} \x1b[1;38;2;240;235;225m{key} {:<name_w$}\x1b[0m \x1b[38;2;225;175;95m{url}\x1b[0m{saved}", p.name)
                    } else {
                        format!("{ptr} \x1b[38;2;160;155;145m{key} {:<name_w$}\x1b[0m \x1b[38;2;135;130;125m{url}\x1b[0m{saved}", p.name)
                    };
                    lines.push(pad_row(&row));
                }
                if end < presets.len() {
                    lines.push(pad_row(&format!("  \x1b[38;2;135;130;125m▼ {} more below\x1b[0m", presets.len() - end)));
                }

                // Custom backend, with an editable field and a dim placeholder.
                let is_custom_sel = self.is_custom();
                let ptr = if is_custom_sel { "\x1b[1;38;2;225;175;95m▸\x1b[0m" } else { " " };
                let placeholder_color = "\x1b[38;2;68;65;62m";
                let placeholder_text = "type its URL";
                let custom_row = if self.custom_url.is_empty() {
                    if is_custom_sel {
                        format!("{ptr} \x1b[1;38;2;240;235;225mc. {:<name_w$}\x1b[0m \x1b[7m \x1b[27m{placeholder_color}{placeholder_text}\x1b[0m", "Custom")
                    } else {
                        format!("{ptr} \x1b[38;2;160;155;145mc. {:<name_w$}\x1b[0m {placeholder_color}{placeholder_text}\x1b[0m", "Custom")
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
                    format!("{ptr} \x1b[1;38;2;240;235;225mc. {:<name_w$}\x1b[0m \x1b[1;38;2;225;175;95m{before}{at_and_after}\x1b[0m", "Custom")
                } else {
                    format!("{ptr} \x1b[38;2;160;155;145mc. {:<name_w$}\x1b[0m \x1b[38;2;135;130;125m{}\x1b[0m", "Custom", self.custom_url)
                };
                lines.push(pad_row(&custom_row));
                lines.push(pad_row(""));
                // What the chosen row is, and how it will be spoken to.
                let about = match self.preset() {
                    Some(p) => format!("{} \u{b7} {}", p.description, p.protocol.label()),
                    None if self.custom_url.is_empty() => "Any server's address, e.g. http://localhost:8080/v1".to_string(),
                    None => format!("Spoken to as {} \u{b7} Tab changes it", self.profile.protocol.label()),
                };
                for row in crate::wrap_styled(&format!("\x1b[38;2;135;130;125m{about}\x1b[0m"), inner_w.saturating_sub(6).max(1)) {
                    lines.push(pad_row(&format!("  {row}")));
                }

                if let Some(ref st) = self.connection_status {
                    lines.push(pad_row(""));
                    lines.push(pad_row(&format!("Status: {st}")));
                }
            }
            1 => {
                lines.push(pad_row(&heading(1, "API key")));
                let env_key = self.env_key();
                if let Some(var) = env_key {
                    lines.push(pad_row(&format!("\x1b[38;2;145;205;140mFound {var} in your environment.\x1b[0m")));
                    lines.push(pad_row(""));
                    lines.push(pad_row("  \x1b[38;2;160;155;145mPress Enter to use it, or paste a key to save one for this provider.\x1b[0m"));
                } else if self.key_required() {
                    lines.push(pad_row("\x1b[38;2;225;175;95mThis provider needs an API key.\x1b[0m"));
                    lines.push(pad_row(""));
                    lines.push(pad_row("  \x1b[38;2;160;155;145mHow to get your API key:\x1b[0m"));
                    if self.profile.url.contains("openrouter.ai") {
                        lines.push(pad_row("    1. Open \x1b[1;38;2;145;205;140mhttps://openrouter.ai/keys\x1b[0m in your browser"));
                        lines.push(pad_row("    2. Create a key and copy it (format: \x1b[38;2;175;170;225msk-or-v1-...\x1b[0m)"));
                        lines.push(pad_row("    3. Paste or type your key below:"));
                    } else {
                        lines.push(pad_row("    1. Open your provider account / API dashboard"));
                        lines.push(pad_row("    2. Copy your Secret / Bearer API token"));
                        lines.push(pad_row("    3. Paste or type your key below:"));
                    }
                    if let Some(var) = self.profile.key_env().first() {
                        lines.push(pad_row(&format!("  \x1b[38;2;135;130;125mOr set {var}: FlashAgent reads it when no key is saved.\x1b[0m")));
                    }
                } else if self.is_cloud_backend() {
                    lines.push(pad_row("\x1b[38;2;225;175;95mA server on the internet usually wants a key.\x1b[0m"));
                    lines.push(pad_row(""));
                    lines.push(pad_row("  \x1b[38;2;160;155;145mPaste it below, or press Enter if this one takes none.\x1b[0m"));
                } else {
                    lines.push(pad_row("\x1b[38;2;145;205;140mA server on your network usually needs no key.\x1b[0m"));
                    lines.push(pad_row(""));
                    lines.push(pad_row("  \x1b[38;2;160;155;145mIf your server uses Bearer auth, enter it below.\x1b[0m"));
                    lines.push(pad_row("  \x1b[38;2;135;130;125mOtherwise, leave empty and press Enter to continue.\x1b[0m"));
                }
                lines.push(pad_row(""));

                let placeholder_color = "\x1b[38;2;68;65;62m";
                let key_display = if self.api_key_input.is_empty() {
                    let placeholder = if env_key.is_some() {
                        "Enter uses the environment's key"
                    } else if self.key_required() {
                        "Paste API key here"
                    } else {
                        "Enter API key here (Enter to skip)"
                    };
                    format!("\x1b[7m \x1b[27m{placeholder_color}{placeholder}\x1b[0m")
                } else {
                    // The length shows that a paste landed, and whether it landed twice.
                    let masked = mask_api_key(&self.api_key_input);
                    let count = crate::plural(self.api_key_input.chars().count(), "character", "characters");
                    format!("\x1b[1;38;2;225;175;95m{masked}\x1b[7m \x1b[27m\x1b[0m \x1b[38;2;135;130;125m({count})\x1b[0m")
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

                lines.push(pad_row(&heading(2, "Choose the model")));
                let count_info = if self.is_lm_studio() && self.discovered_models.iter().any(|m| m.is_loaded) {
                    format!("({total_models} loaded in LM Studio)")
                } else if self.model_search.is_empty() {
                    format!("({total_models} on the server)")
                } else {
                    format!("({total_matches}/{total_models} · filter: \"{}\")", self.model_search)
                };
                lines.push(pad_row(&format!("\x1b[38;2;160;155;145mChoose model \x1b[38;2;175;170;225m{count_info}\x1b[0m:\x1b[0m")));
                lines.push(pad_row(""));

                if self.available_models.is_empty() {
                    // Said plainly: a missing server is the usual first-run problem.
                    let url = self.profile.url.trim_end_matches('/');
                    let (said, next) = match self.connection_status.as_deref() {
                        Some(status) if status.starts_with("Connecting") => (status.to_string(), String::new()),
                        Some(status) if !status.starts_with("Connected") => (
                            format!("No server answered at {url}."),
                            "Start it and press Esc to try again, or Enter to go on and pick a model later with F3.".to_string(),
                        ),
                        _ => (
                            format!("{url} lists no models."),
                            "Load one in your server and press Esc to look again, or Enter to go on.".to_string(),
                        ),
                    };
                    lines.push(pad_row(&format!("  \x1b[38;2;225;175;95m{said}\x1b[0m")));
                    for row in crate::wrap_styled(&next, inner_w.saturating_sub(6)) {
                        lines.push(pad_row(&format!("  \x1b[38;2;160;155;145m{row}\x1b[0m")));
                    }
                } else if filtered.is_empty() {
                    lines.push(pad_row("  \x1b[38;2;135;130;125m(No models matching search filter)\x1b[0m"));
                } else {
                    let cur_pos = filtered.iter().position(|&idx| idx == self.model_idx).unwrap_or(0);
                    let has_metadata = !self.discovered_models.is_empty();
                    let page_size = if has_metadata { 5 } else { 10 };
                    let start = if cur_pos < page_size { 0 } else { (cur_pos + 1).saturating_sub(page_size) };
                    let end = (start + page_size).min(total_matches);

                    if start > 0 {
                        lines.push(pad_row(&format!("  \x1b[38;2;135;130;125m▲ {start} more above\x1b[0m")));
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
                        lines.push(pad_row(&format!("  \x1b[38;2;135;130;125m▼ {} more below\x1b[0m", total_matches - end)));
                    }
                }
            }
            3 => {
                lines.push(pad_row(&heading(3, "Permissions")));
                lines.push(pad_row("\x1b[38;2;160;155;145mWhat may the agent do without asking? Shift+Tab changes it any time.\x1b[0m"));
                lines.push(pad_row(""));
                let modes = [
                    (PermissionMode::Planning, "1. Planning", "only reads and plans; changes nothing"),
                    (PermissionMode::Manual, "2. Manual", "asks before every edit and command"),
                    (PermissionMode::AcceptEdits, "3. Accept Edits", "edits files, asks before commands (default)"),
                    (PermissionMode::Bypass, "4. Accept All", "edits and runs commands without asking"),
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
                lines.push(pad_row(&heading(4, "Sampling and launch")));
                lines.push(pad_row("\x1b[38;2;160;155;145mHow the model samples. The preset suits most models; its numbers are in /sampling.\x1b[0m"));
                lines.push(pad_row(""));

                let preset_val = format!("[ {} ]", self.sampling.preset.label());
                lines.push(pad_row(&format!(
                    "\x1b[1;38;2;225;175;95m▸\x1b[0m \x1b[1;38;2;240;235;225m{:<18}\x1b[0m \x1b[1;38;2;225;175;95m{:<22}\x1b[0m",
                    "Preset", preset_val
                )));

                lines.push(pad_row(""));
                lines.push(pad_row(" \x1b[1;38;2;25;30;22;48;2;145;205;140m Save and launch \x1b[0m"));
            }
            _ => {}
        }

        lines.push(pad_row(""));
        let hint_w = inner_w.saturating_sub(2);
        let model_enter = if self.provider_only { "save" } else { "choose" };
        let hints: Vec<(&str, &str)> = match self.step {
            0 if self.is_custom() => vec![("type", "the URL"), ("Tab", "protocol"), ("\u{2191}/\u{2193}", "presets"), ("Enter", "next"), ("Esc", "clear or quit")],
            0 => vec![("\u{2191}/\u{2193}", "move"), ("0-9 c", "pick"), ("Enter", "next"), ("Esc", "quit")],
            1 if self.key_required() => vec![("paste", "the key"), ("Enter", "check it"), ("Esc", "clear or back")],
            1 => vec![("Enter", "skip"), ("Esc", "back")],
            2 => vec![("\u{2191}/\u{2193}", "move"), ("type", "to filter"), ("Enter", model_enter), ("Esc", "clear or back")],
            3 => vec![("\u{2191}/\u{2193}", "move"), ("1-4", "pick"), ("Enter", "next"), ("Esc", "back")],
            4 => vec![("\u{2190}/\u{2192}", "preset"), ("Enter", "launch"), ("Esc", "back")],
            _ => Vec::new(),
        };
        if !hints.is_empty() {
            lines.push(pad_row(&crate::key_hints(&hints, hint_w)));
        }

        lines.push(format!("  {border_color}╰{}╯{reset}", "─".repeat(inner_w)));
        lines
    }
}

/// Discovery with the provider as set up so far: its address, protocol and
/// key, typed or from the environment.
async fn probe_server(wizard: &mut SetupWizard) {
    let backend = flashagent_llm::Client::new(wizard.profile.endpoint(), "");
    if let Some(disc) = backend.discover_server().await {
        wizard.apply_discovered_models(disc);
    } else {
        wizard.available_models.clear();
        wizard.discovered_models.clear();
        wizard.connection_status = Some("No server answered".to_string());
    }
}

/// On arriving at the model step from before it: the server is asked with
/// what the steps before gave.
async fn after_key(wizard: &mut SetupWizard, prev_step: usize, painter: &mut crate::screen::Screen) {
    if prev_step < 2 && wizard.step == 2 {
        wizard.available_models.clear();
        wizard.connection_status = Some(format!("Connecting to {}\u{2026}", wizard.profile.url.trim_end_matches('/')));
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        crate::screen::paint_page(painter, &wizard.render(term_w as usize), true);
        probe_server(wizard).await;
    }
}

/// Inside the running app: reads keys from the event channel, so it does not
/// compete with the main reader for stdin. Everything else on `rx` (turn
/// events, discovery, recap) goes into `deferred` in order; dropping it left a
/// turn that finished meanwhile running forever. `adding` asks only for a
/// provider, beside the saved ones.
pub async fn run_wizard_channel(
    config: &mut AppConfig,
    adding: bool,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::UiEvent>,
    deferred: &mut Vec<crate::UiEvent>,
) -> anyhow::Result<bool> {
    use crossterm::{cursor, execute};

    // Already on the alternate screen: entering and leaving again would drop the
    // app onto the main screen.
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, cursor::Hide);

    let mut wizard = if adding { SetupWizard::adding(config.clone()) } else { SetupWizard::new(config.clone()) };
    if !adding {
        let backend = flashagent_llm::Client::new(wizard.profile.endpoint(), "");
        if let Some(disc) = backend.discover_server().await {
            wizard.apply_discovered_models(disc);
        }
    }

    let mut painter = crate::screen::Screen::new();
    let completed = loop {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        crate::screen::paint_page(&mut painter, &wizard.render(term_w as usize), true);

        tokio::select! {
            Some(ev) = rx.recv() => {
                match ev {
                    crate::UiEvent::Key(code, mods) => {
                        let prev_step = wizard.step;
                        if let Some(finished) = wizard.handle_key(code, mods) {
                            break finished;
                        }
                        after_key(&mut wizard, prev_step, &mut painter).await;
                    }
                    crate::UiEvent::Paste(text) => {
                        wizard.handle_paste(&text);
                    }
                    crate::UiEvent::Resize(_, _) | crate::UiEvent::Mouse(_) => {}
                    other => deferred.push(other),
                }
            }
        }
    };

    if completed {
        *config = wizard.config;
    }

    // The app repaints every row over what is left; raw mode stays on, run_app
    // is still active.
    let _ = execute!(stdout, cursor::Show);
    Ok(completed)
}

/// For the standalone startup flow.
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

    let backend = flashagent_llm::Client::new(wizard.profile.endpoint(), "");
    if let Some(disc) = backend.discover_server().await {
        wizard.apply_discovered_models(disc);
    }

    let mut painter = crate::screen::Screen::new();
    let completed = loop {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        crate::screen::paint_page(&mut painter, &wizard.render(term_w as usize), true);

        if crossterm::event::poll(std::time::Duration::from_millis(50))? {
            match crossterm::event::read()? {
                Event::Key(key) if key.kind != KeyEventKind::Release => {
                    let prev_step = wizard.step;
                    if let Some(finished) = wizard.handle_key(key.code, key.modifiers) {
                        break finished;
                    }
                    after_key(&mut wizard, prev_step, &mut painter).await;
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

    fn with_provider(url: &str) -> AppConfig {
        let mut cfg = AppConfig::default();
        cfg.save_provider(ProviderProfile::from_url(url));
        cfg
    }

    fn preset_row(name: &str) -> usize {
        BackendPreset::all().iter().position(|p| p.name == name).unwrap()
    }

    #[test]
    fn test_wizard_steps_and_navigation() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);
        assert_eq!(wizard.step, 0);

        wizard.next_option();
        assert_eq!(wizard.preset_idx, 1);

        // Ollama runs on this computer: no key to ask for.
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 2);
        assert!(wizard.render(80).join("\n").contains("Step 2: Choose the model"));
        assert_eq!(wizard.total_steps(), 4);

        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 3);
        assert_eq!(wizard.config.permission_mode, PermissionMode::AcceptEdits);
        wizard.pick_key('1');
        assert_eq!(wizard.config.permission_mode, PermissionMode::Planning);
        wizard.pick_key('3');
        assert_eq!(wizard.config.permission_mode, PermissionMode::AcceptEdits);

        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 4);

        wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(wizard.step, 3);

        wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(wizard.step, 2);

        wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(wizard.step, 0, "back past the key step it never showed");

        // On step 0, Esc exits.
        let res = wizard.handle_key(KeyCode::Esc, KeyModifiers::empty());
        assert_eq!(res, Some(false));
    }

    #[test]
    fn test_wizard_cloud_mandatory_api_key() {
        let mut wizard = SetupWizard::new(with_provider("https://openrouter.ai/api/v1"));
        wizard.step = 1; // On API key step

        assert!(wizard.is_cloud_backend());

        // An empty key must not advance, unless the environment has one.
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        if wizard.env_key().is_none() {
            assert_eq!(wizard.step, 1);
            assert!(wizard.connection_status.as_ref().unwrap().contains("API key is required"));
        }
        wizard.step = 1;

        for c in "sk-or-v1-abcdef123456".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        assert_eq!(wizard.api_key_input, "sk-or-v1-abcdef123456");

        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("sk-or-••••••••3456"));
        assert!(!rendered.contains("abcdef"), "the key is masked");

        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 2);
        assert_eq!(wizard.profile.api_key.as_deref(), Some("sk-or-v1-abcdef123456"));
    }

    #[test]
    fn test_wizard_model_scrolling_and_live_search() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);
        wizard.step = 2;
        wizard.available_models = (0..430)
            .map(|i| format!("provider/model-variant-{i:03}"))
        .collect();

        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("430 on the server"));
        assert!(rendered.contains("▼ 420 more below"));

        for c in "variant-04".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        assert_eq!(wizard.model_search, "variant-04");
        let search_rendered = wizard.render(80).join("\n");
        assert!(search_rendered.contains("filter: \"variant-04\""));

        wizard.handle_key(KeyCode::PageDown, KeyModifiers::empty());

        while !wizard.model_search.is_empty() {
            wizard.handle_key(KeyCode::Backspace, KeyModifiers::empty());
        }
        assert_eq!(wizard.model_search, "");
    }

    #[test]
    fn test_wizard_custom_backend_selection_and_typing() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);

        assert_eq!(wizard.custom_url, "");
        let initial_render = wizard.render(80).join("\n");
        assert!(initial_render.contains("type its URL"));

        wizard.handle_key(KeyCode::Char('c'), KeyModifiers::empty());
        assert!(wizard.is_custom());
        let selected_render = wizard.render(80).join("\n");
        assert!(selected_render.contains("type its URL"));
        assert!(selected_render.contains("Any server's address"));

        // Enter on an empty URL does not advance and shows a warning.
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 0);
        assert!(wizard.connection_status.as_ref().unwrap().contains("Please enter an endpoint URL"));

        wizard.handle_key(KeyCode::Char('h'), KeyModifiers::empty());
        assert_eq!(wizard.custom_url, "h");
        let typed_render = wizard.render(80).join("\n");
        assert!(!typed_render.contains("type its URL"));
        assert!(typed_render.contains("h"));

        for c in "ttp://192.168.1.50:5000/v1".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        assert_eq!(wizard.custom_url, "http://192.168.1.50:5000/v1");
        assert_eq!(wizard.profile.url, "http://192.168.1.50:5000/v1");

        while !wizard.custom_url.is_empty() {
            wizard.handle_key(KeyCode::Backspace, KeyModifiers::empty());
        }
        assert_eq!(wizard.custom_url, "");
        let emptied_render = wizard.render(80).join("\n");
        assert!(emptied_render.contains("type its URL"));

        for c in "http://192.168.1.50:5000/v1".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1, "an address typed by hand may still want a key");
        assert_eq!(wizard.profile.url, "http://192.168.1.50:5000/v1");
        assert_eq!(wizard.profile.name, "192.168.1.50:5000");
    }

    #[test]
    fn a_typed_address_never_insists_on_a_key() {
        for url in ["http://192.168.1.50:8000/v1", "https://llm.example.org/v1"] {
            let mut wizard = SetupWizard::new(AppConfig::default());
            wizard.handle_key(KeyCode::Char('c'), KeyModifiers::empty());
            wizard.handle_paste(url);
            wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
            assert_eq!(wizard.step, 1, "{url}");
            wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
            assert_eq!(wizard.step, 2, "{url}: an empty key was refused");
        }
        let mut wizard = SetupWizard::new(AppConfig::default());
        wizard.handle_key(KeyCode::Char('c'), KeyModifiers::empty());
        wizard.handle_paste("http://192.168.1.50:8000/v1");
        assert!(!wizard.is_cloud_backend(), "plain http is a server on the network");
    }

    #[test]
    fn a_custom_address_takes_the_protocol_it_implies_and_tab_changes_it() {
        let mut wizard = SetupWizard::new(AppConfig::default());
        wizard.handle_key(KeyCode::Char('c'), KeyModifiers::empty());
        wizard.handle_paste("http://gpu-box:11434");
        assert_eq!(wizard.profile.protocol, ApiProtocol::Ollama, "read from the address");
        assert!(crate::strip_ansi(&wizard.render(100).join("\n")).contains("Spoken to as Ollama \u{b7} Tab changes it"));
        wizard.handle_key(KeyCode::Tab, KeyModifiers::empty());
        assert_eq!(wizard.profile.protocol, ApiProtocol::OpenAi, "Tab goes round to the first");
        wizard.handle_key(KeyCode::Tab, KeyModifiers::empty());
        assert_eq!(wizard.profile.protocol, ApiProtocol::Anthropic);
        wizard.handle_key(KeyCode::Char('/'), KeyModifiers::empty());
        assert_eq!(wizard.profile.protocol, ApiProtocol::Anthropic, "typing keeps the chosen protocol");
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);
        assert_eq!(wizard.profile.url, "http://gpu-box:11434");
    }

    #[test]
    fn test_wizard_custom_navigation_and_arrows() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);

        wizard.handle_key(KeyCode::Char('c'), KeyModifiers::empty());
        assert!(wizard.is_custom());

        let presets = BackendPreset::all();
        wizard.handle_key(KeyCode::Up, KeyModifiers::empty());
        assert_eq!(wizard.preset_idx, presets.len() - 1);
        assert_eq!(wizard.profile.url, presets[presets.len() - 1].url);

        wizard.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert!(wizard.is_custom());
    }

    #[test]
    fn every_preset_can_be_reached_and_none_is_mistaken_for_custom() {
        let presets = BackendPreset::all();
        let mut wizard = SetupWizard::new(AppConfig::default());
        // The first ten have a key of their own.
        for (i, preset) in presets.iter().enumerate().take(10) {
            let key = SetupWizard::row_key(i).expect("the first ten have a key");
            wizard.handle_key(KeyCode::Char(key), KeyModifiers::empty());
            assert_eq!(wizard.preset_idx, i, "{}", preset.name);
            assert!(!wizard.is_custom());
            assert_eq!((wizard.profile.url.as_str(), wizard.profile.protocol), (preset.url, preset.protocol));
        }
        // The arrows reach every row, show it, and come back round.
        let mut wizard = SetupWizard::new(AppConfig::default());
        for preset in presets.iter().skip(1) {
            wizard.handle_key(KeyCode::Down, KeyModifiers::empty());
            assert_eq!(wizard.profile.name, preset.name);
            let shown = crate::strip_ansi(&wizard.render(100).join("\n"));
            assert!(shown.contains(preset.url), "{} is selected but not shown:\n{shown}", preset.name);
        }
        wizard.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert!(wizard.is_custom());
        wizard.handle_key(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(wizard.preset_idx, 0);
        // A saved cloud preset opens on its own row.
        let last = presets.len() - 1;
        let saved = with_provider(presets[last].url);
        assert_eq!(SetupWizard::new(saved).preset_idx, last);
        let rendered = SetupWizard::new(AppConfig::default()).render(100).join("\n");
        assert_eq!(rendered.matches("c. Custom").count(), 1, "{rendered}");
        assert!(rendered.contains("0. OpenAI"), "{rendered}");
        assert!(rendered.contains("more below"), "{rendered}");
    }

    #[test]
    fn the_preset_list_fits_a_small_terminal() {
        for row in [0, 9, 14, BackendPreset::all().len()] {
            let mut wizard = SetupWizard::new(AppConfig::default());
            wizard.select_row(row);
            let rows = wizard.render(80);
            assert!(rows.len() < 24, "{} rows and the blank one above them at 80x24 with row {row} chosen", rows.len());
            assert!(rows.iter().all(|r| crate::visible_width(r) <= 80));
        }
    }

    #[test]
    fn a_local_preset_skips_the_key_and_a_cloud_one_notices_its_environment_variable() {
        let mut wizard = SetupWizard::new(AppConfig::default());
        wizard.select_row(preset_row("llama.cpp"));
        assert!(!wizard.needs_key_step());
        wizard.select_row(preset_row("Anthropic"));
        assert!(wizard.needs_key_step());
        assert_eq!(wizard.total_steps(), 5);
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);
        let shown = crate::strip_ansi(&wizard.render(100).join("\n"));
        match wizard.env_key() {
            Some(var) => assert!(shown.contains(&format!("Found {var} in your environment.")), "{shown}"),
            None => assert!(shown.contains("Or set ANTHROPIC_API_KEY: FlashAgent reads it when no key is saved."), "{shown}"),
        }
    }

    #[test]
    fn running_again_adds_a_provider_and_keeps_the_saved_ones() {
        let mut cfg = with_provider("http://localhost:1234/v1");
        cfg.active_profile_mut().model = "local-model".into();
        cfg.setup_completed = true;

        let mut wizard = SetupWizard::adding(cfg.clone());
        assert_eq!(wizard.total_steps(), 2, "a local server: server and model");
        wizard.select_row(preset_row("vLLM"));
        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        wizard.available_models = vec!["served-model".into()];
        assert_eq!(wizard.handle_key(KeyCode::Enter, KeyModifiers::empty()), Some(true), "the model step ends it");
        let names: Vec<&str> = wizard.config.providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["LM Studio", "vLLM"]);
        assert_eq!(wizard.config.active_profile().name, "vLLM");
        assert_eq!(wizard.config.active_profile().model, "served-model");

        // The same server again updates it instead of adding a twin, and keeps its key.
        let mut again = SetupWizard::adding(wizard.config.clone());
        again.select_row(0);
        assert_eq!(again.profile.model, "local-model", "the saved provider for that row is loaded");
        again.handle_key(KeyCode::Enter, KeyModifiers::empty());
        again.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(again.config.providers.len(), 2);
        assert_eq!(again.config.active_profile().name, "LM Studio");
        assert!(crate::strip_ansi(&SetupWizard::adding(again.config.clone()).render(100).join("\n")).contains("saved"));
    }

    #[test]
    fn a_pasted_key_loses_the_whitespace_it_was_copied_with_and_shows_its_length() {
        let mut wizard = SetupWizard::new(AppConfig::default());
        wizard.step = 1;
        wizard.handle_paste(" sk-or-v1-abc def\r\n");
        assert_eq!(wizard.api_key_input, "sk-or-v1-abcdef");
        let shown = crate::strip_ansi(&wizard.render(100).join("\n"));
        assert!(shown.contains("(15 characters)"), "{shown}");
    }

    #[test]
    fn a_key_that_is_not_ascii_is_masked_without_a_panic() {
        assert_eq!(mask_api_key("ыл-а1а"), "••••••");
        let masked = mask_api_key("sk-abcdeабвгд1234");
        assert!(masked.starts_with("sk-") && masked.ends_with("1234"), "{masked}");
        assert_eq!(mask_api_key("ключ-длинный-ключ"), "ключ••••••••ключ");
    }

    #[test]
    fn test_wizard_custom_auto_prefix_http() {
        let cfg = AppConfig::default();
        let mut wizard = SetupWizard::new(cfg);

        wizard.handle_key(KeyCode::Char('c'), KeyModifiers::empty());

        // Without http://
        for c in "localhost:8000/v1".chars() {
            wizard.handle_key(KeyCode::Char(c), KeyModifiers::empty());
        }

        wizard.handle_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(wizard.step, 1);
        assert_eq!(wizard.profile.url, "http://localhost:8000/v1");
        assert_eq!(wizard.profile.name, "vLLM", "named after the server at that address");
    }

    #[test]
    fn test_wizard_preserves_existing_custom_url() {
        let wizard = SetupWizard::new(with_provider("http://10.0.0.5:1234/v1"));
        assert!(wizard.is_custom());
        assert_eq!(wizard.custom_url, "http://10.0.0.5:1234/v1");
        assert_eq!(wizard.custom_cursor, "http://10.0.0.5:1234/v1".len());
        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("http://10.0.0.5:1234/v1"));
        assert!(!rendered.contains("type its URL"));
    }

    #[test]
    fn test_wizard_lm_studio_loaded_models_filtering_and_capabilities() {
        let mut wizard = SetupWizard::new(with_provider("http://127.0.0.1:1234/v1"));
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

        wizard.apply_discovered_models(flashagent_llm::ServerDiscovery {
            base_url: wizard.profile.url.clone(),
            models,
            active_model: None,
            kind: flashagent_llm::thinking::ServerKind::LmStudio,
        });

        // Only the loaded model is kept.
        assert_eq!(wizard.available_models.len(), 1);
        assert_eq!(wizard.available_models[0], "deepseek-r1-distill-qwen-14b");
        assert_eq!(wizard.profile.model, "deepseek-r1-distill-qwen-14b");

        wizard.step = 2;
        let rendered = wizard.render(80).join("\n");
        assert!(rendered.contains("(1 loaded in LM Studio)"));
        assert!(rendered.contains("deepseek-r1-distill-qwen-14b"));
        assert!(rendered.contains("128k ctx · tools · thinking: on [off,on]"));
    }

    #[test]
    fn test_wizard_handle_paste_instantly_updates_fields() {
        let mut wizard = SetupWizard::new(AppConfig::default());
        wizard.select_row(SetupWizard::custom_idx());

        wizard.handle_paste("http://192.168.1.50:8000/v1\r\n");
        assert_eq!(wizard.custom_url, "http://192.168.1.50:8000/v1");
        assert_eq!(wizard.profile.url, "http://192.168.1.50:8000/v1");

        wizard.step = 1;
        wizard.handle_paste("sk-paste-token-xyz");
        assert_eq!(wizard.api_key_input, "sk-paste-token-xyz");
        assert_eq!(wizard.profile.api_key.as_deref(), Some("sk-paste-token-xyz"));

        wizard.step = 2;
        wizard.available_models = vec![
            "llama-3.1-8b-instruct".to_string(),
            "deepseek-coder-v2".to_string(),
            "qwen2.5-coder-7b".to_string(),
        ];
        wizard.handle_paste("coder");
        assert_eq!(wizard.model_search, "coder");
        assert_eq!(wizard.profile.model, "deepseek-coder-v2");
    }
}
