//! Saved providers: the `/provider` menu that switches between them, and the
//! screen that edits them. Switching itself (a new endpoint, discovery, the
//! model) is the app's; this module decides what to switch to.

use crossterm::event::{KeyCode, KeyModifiers};
use flashagent_core::config::{AppConfig, KeySource, ProviderProfile};
use flashagent_llm::{ApiProtocol, ServerDiscovery};

use crate::{LineKind, RenderLine, SelectItem, SelectMenu};

/// Menu values that are actions, not provider names; a name typed on a
/// keyboard never starts with a NUL.
pub const ADD: &str = "\u{0}add";
pub const MANAGE: &str = "\u{0}manage";
/// The `--url` server of this run, which has no saved name to switch to.
pub const THIS_RUN: &str = "\u{0}run";

/// Said instead of switching while a turn runs: the turn's requests would
/// go to one server and its answer be read as another's.
pub const BUSY: &str = "Wait for the answer to finish, then switch provider";

/// Where a provider's key comes from, never the key.
pub fn key_status(profile: &ProviderProfile) -> String {
    match profile.key_source() {
        KeySource::Saved => "key saved".to_string(),
        KeySource::Env(var) => format!("key from {var}"),
        KeySource::None => match profile.key_env().first() {
            Some(var) => format!("no key yet ({var})"),
            None => "no key".to_string(),
        },
    }
}

/// What `/provider ` completes: each saved name and how it is reached.
pub fn completion_entries(config: &AppConfig) -> Vec<(String, String)> {
    config.providers.iter().map(|p| (p.name.clone(), describe(p))).collect()
}

/// How a provider is spoken to, where, and the model it starts with.
pub fn describe(profile: &ProviderProfile) -> String {
    let model = if profile.model.is_empty() { "the loaded model" } else { profile.model.as_str() };
    format!("{} \u{b7} {} \u{b7} {model}", profile.protocol.label(), profile.url)
}

/// `/provider`: the saved providers, the one in use marked, then adding one
/// and editing them.
pub fn provider_menu(config: &AppConfig) -> SelectMenu<String> {
    let in_use = config.active_profile();
    let mut items = Vec::new();
    if let Some(run) = &config.run_provider {
        items.push(SelectItem::with_description(format!("\u{25cf} {}  (--url, this run only)", run.name), describe(run), THIS_RUN.to_string()));
    }
    for p in &config.providers {
        let label = if config.run_provider.is_none() && p.name == in_use.name {
            format!("\u{25cf} {}  (in use)", p.name)
        } else {
            format!("\u{25cb} {}", p.name)
        };
        items.push(SelectItem::with_description(label, describe(p), p.name.clone()));
    }
    items.push(SelectItem::with_description("+ Add a provider", "a local server or a cloud API, set up step by step", ADD.to_string()));
    items.push(SelectItem::with_description("+ Edit providers", "address, protocol, key and model of each; delete one", MANAGE.to_string()));
    let mut menu = SelectMenu::new("Switch provider", items).with_noun("providers");
    menu.uncounted = 2;
    menu.select_by_value(&if config.run_provider.is_some() { THIS_RUN.to_string() } else { in_use.name.clone() });
    menu
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderArg {
    Switch(String),
    /// Open the menu, filtered by this.
    Menu(String),
}

/// `/provider <text>`: a saved name, in any case, switches at once; anything
/// else narrows the menu, as `/model <text>` does.
pub fn provider_arg(config: &AppConfig, arg: &str) -> ProviderArg {
    let arg = arg.trim();
    let wanted = arg.to_lowercase();
    match config.providers.iter().find(|p| !wanted.is_empty() && p.name.to_lowercase() == wanted) {
        Some(p) => ProviderArg::Switch(p.name.clone()),
        None => ProviderArg::Menu(arg.to_string()),
    }
}

/// The model to go on with, at start or after a switch: the one saved for the
/// provider if the server lists it, else the one it has loaded, else its
/// first. Without an answer, the saved one.
///
/// Matched exactly first (Gemini's `models/` prefix aside), then as part of a
/// longer id (`qwen3` finds `qwen3-coder-30b`), and only then as a longer name
/// holding an id, the closest first: a saved `gemini-2.5-flash-lite` must not
/// become `gemini-2.5-flash` because that is listed first.
pub fn model_after_switch(saved: &str, disc: Option<&ServerDiscovery>) -> String {
    let Some(disc) = disc else { return saved.to_string() };
    let bare = |id: &str| id.strip_prefix("models/").unwrap_or(id).to_ascii_lowercase();
    let wanted = bare(saved);
    if !wanted.is_empty() {
        let exact = disc.models.iter().find(|m| bare(&m.id) == wanted);
        let longer = || disc.models.iter().filter(|m| bare(&m.id).contains(&wanted)).min_by_key(|m| m.id.len());
        let shorter = || disc.models.iter().filter(|m| wanted.contains(&bare(&m.id))).max_by_key(|m| m.id.len());
        if let Some(m) = exact.or_else(longer).or_else(shorter) {
            return m.id.clone();
        }
    }
    disc.active_model
        .as_ref()
        .or_else(|| disc.models.first())
        .map_or_else(|| saved.to_string(), |m| m.id.clone())
}

/// What F3 and Settings offer: on LM Studio only the loaded models when any
/// are, since the rest would first have to load.
pub fn offered_models(disc: &ServerDiscovery) -> Vec<String> {
    let lm_studio = disc.kind == flashagent_llm::thinking::ServerKind::LmStudio;
    let any_loaded = disc.models.iter().any(|m| m.is_loaded);
    disc.models.iter().filter(|m| !(lm_studio && any_loaded) || m.is_loaded).map(|m| m.id.clone()).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvidersAction {
    None,
    Close,
    Switch(String),
    /// Through the setup steps, which know the presets and ask the server.
    Add,
    /// `original` is the name it had.
    Save { original: String, profile: ProviderProfile },
    Delete(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Protocol,
    Address,
    Key,
    Model,
    /// Only where the client picks the window (Ollama's `num_ctx`).
    Context,
    Save,
}

const FIELDS: [Field; 6] = [Field::Name, Field::Protocol, Field::Address, Field::Key, Field::Model, Field::Save];
const FIELDS_WITH_CONTEXT: [Field; 7] = [Field::Name, Field::Protocol, Field::Address, Field::Key, Field::Model, Field::Context, Field::Save];

fn fields(profile: &ProviderProfile) -> &'static [Field] {
    if profile.protocol == ApiProtocol::Ollama { &FIELDS_WITH_CONTEXT } else { &FIELDS }
}

/// "32768", "32k", "128K" or "1m"; `None` for empty (FlashAgent's choice) or nonsense.
fn parse_context(text: &str) -> Option<usize> {
    let text = text.trim().to_ascii_lowercase().replace(['_', ','], "");
    let (digits, scale) = match text.strip_suffix('k') {
        Some(d) => (d, 1024),
        None => match text.strip_suffix('m') {
            Some(d) => (d, 1024 * 1024),
            None => (text.as_str(), 1),
        },
    };
    digits.trim().parse::<usize>().ok().map(|n| n * scale).filter(|&n| n > 0)
}

#[derive(Debug, Clone)]
struct Form {
    original: String,
    draft: ProviderProfile,
    row: usize,
    /// What is being typed into the selected field. A key starts empty: the
    /// saved one is never put on screen.
    typing: Option<String>,
}

/// Settings -> Providers, and "Edit providers" in `/provider`: every saved
/// provider, to switch to, edit, delete or add.
pub struct ProvidersView {
    pub profiles: Vec<ProviderProfile>,
    /// The name of the one in use.
    pub active: String,
    /// A row of the list; one past the last provider is "Add a provider".
    pub selected: usize,
    form: Option<Form>,
    confirm_delete: bool,
    /// A refusal or a result, under the list.
    pub status: Option<String>,
}

/// Rows of the list shown at once; each provider takes two.
const LIST_WINDOW: usize = 6;

fn cycle_protocol(p: ApiProtocol, forward: bool) -> ApiProtocol {
    let all = ApiProtocol::ALL;
    let i = all.iter().position(|x| *x == p).unwrap_or(0);
    all[if forward { (i + 1) % all.len() } else { (i + all.len() - 1) % all.len() }]
}

impl ProvidersView {
    pub fn new(config: &AppConfig) -> Self {
        let mut view = Self { profiles: Vec::new(), active: String::new(), selected: 0, form: None, confirm_delete: false, status: None };
        view.refresh(config);
        view.selected = view.profiles.iter().position(|p| p.name == view.active).unwrap_or(0);
        view
    }

    /// After the app changed the providers: the selection stays on the same
    /// name where it can.
    pub fn refresh(&mut self, config: &AppConfig) {
        let selected_name = self.profiles.get(self.selected).map(|p| p.name.clone());
        self.profiles = config.providers.clone();
        self.active = config.active_profile().name.clone();
        self.selected = selected_name
            .and_then(|name| self.profiles.iter().position(|p| p.name == name))
            .unwrap_or(self.selected.min(self.profiles.len()));
    }

    /// A saved edit: back to the list.
    pub fn saved(&mut self, config: &AppConfig, name: &str) {
        self.form = None;
        self.refresh(config);
        self.selected = self.profiles.iter().position(|p| p.name == name).unwrap_or(self.selected);
        self.status = Some(format!("Saved {name}"));
    }

    /// The edit stays open to be put right.
    pub fn refused(&mut self, why: impl Into<String>) {
        self.status = Some(why.into());
    }

    pub fn is_editing(&self) -> bool {
        self.form.is_some()
    }

    fn selected_profile(&self) -> Option<&ProviderProfile> {
        self.profiles.get(self.selected)
    }

    pub fn handle_paste(&mut self, text: &str) {
        if let Some(typing) = self.form.as_mut().and_then(|f| f.typing.as_mut()) {
            // A key or an address copied from a page often brings a space or a line break.
            typing.extend(text.chars().filter(|c| !c.is_control() && !c.is_whitespace()));
        }
    }

    pub fn handle_key(&mut self, code: KeyCode, mods: KeyModifiers) -> ProvidersAction {
        if self.form.is_some() {
            return self.form_key(code, mods);
        }
        if self.confirm_delete {
            self.confirm_delete = false;
            self.status = None;
            return match (code, self.selected_profile()) {
                (KeyCode::Enter | KeyCode::Char('y' | 'Y' | '\u{043d}'), Some(p)) => ProvidersAction::Delete(p.name.clone()),
                _ => ProvidersAction::None,
            };
        }
        let rows = self.profiles.len() + 1;
        let plain = !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match code {
            KeyCode::Esc => return ProvidersAction::Close,
            KeyCode::Up => self.selected = (self.selected + rows - 1) % rows,
            KeyCode::Down | KeyCode::Tab => self.selected = (self.selected + 1) % rows,
            KeyCode::Enter => {
                return match self.selected_profile() {
                    Some(p) => ProvidersAction::Switch(p.name.clone()),
                    None => ProvidersAction::Add,
                };
            }
            KeyCode::Char('a' | 'A' | '\u{0444}') if plain => return ProvidersAction::Add,
            KeyCode::Char('e' | 'E' | '\u{0443}') if plain => self.start_edit(),
            KeyCode::Delete => self.ask_delete(),
            KeyCode::Char('d' | 'D' | '\u{0432}') if plain => self.ask_delete(),
            _ => {}
        }
        ProvidersAction::None
    }

    fn start_edit(&mut self) {
        if let Some(p) = self.selected_profile().cloned() {
            self.status = None;
            self.form = Some(Form { original: p.name.clone(), draft: p, row: 0, typing: None });
        }
    }

    fn ask_delete(&mut self) {
        let Some(p) = self.selected_profile() else { return };
        if p.name == self.active {
            self.status = Some(format!("{} is in use; switch to another provider before deleting it", p.name));
        } else {
            self.status = Some(format!("Delete {}? Enter deletes it, any other key keeps it", p.name));
            self.confirm_delete = true;
        }
    }

    fn form_key(&mut self, code: KeyCode, mods: KeyModifiers) -> ProvidersAction {
        let Some(form) = self.form.as_mut() else { return ProvidersAction::None };
        let field = fields(&form.draft)[form.row];
        if let Some(typing) = form.typing.as_mut() {
            match code {
                KeyCode::Esc => form.typing = None,
                KeyCode::Backspace => {
                    typing.pop();
                }
                KeyCode::Enter => {
                    let text = typing.trim().to_string();
                    match field {
                        Field::Name if !text.is_empty() => form.draft.name = text,
                        Field::Address if !text.is_empty() => {
                            form.draft.url = if text.contains("://") { text } else { format!("http://{text}") };
                        }
                        Field::Key => form.draft.api_key = (!text.is_empty()).then_some(text),
                        Field::Model => form.draft.model = text,
                        Field::Context => form.draft.context_window = parse_context(&text),
                        _ => {}
                    }
                    form.typing = None;
                }
                KeyCode::Char(c) if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                    // A key never has a space in it; one pasted as keys brings them along.
                    if !(field == Field::Key && c.is_whitespace()) {
                        typing.push(c);
                    }
                }
                _ => {}
            }
            return ProvidersAction::None;
        }
        match code {
            KeyCode::Esc => {
                self.form = None;
                self.status = None;
            }
            KeyCode::Up => {
                let n = fields(&form.draft).len();
                form.row = (form.row + n - 1) % n;
            }
            KeyCode::Down | KeyCode::Tab => form.row = (form.row + 1) % fields(&form.draft).len(),
            KeyCode::Left | KeyCode::Right if field == Field::Protocol => {
                form.draft.protocol = cycle_protocol(form.draft.protocol, code == KeyCode::Right);
            }
            KeyCode::Enter => match field {
                Field::Protocol => form.draft.protocol = cycle_protocol(form.draft.protocol, true),
                Field::Save => {
                    return ProvidersAction::Save { original: form.original.clone(), profile: form.draft.clone() };
                }
                Field::Name => form.typing = Some(form.draft.name.clone()),
                Field::Address => form.typing = Some(form.draft.url.clone()),
                Field::Model => form.typing = Some(form.draft.model.clone()),
                Field::Context => form.typing = Some(form.draft.context_window.map(|n| n.to_string()).unwrap_or_default()),
                Field::Key => form.typing = Some(String::new()),
            },
            _ => {}
        }
        ProvidersAction::None
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        const GOLD: &str = "\x1b[1;38;2;225;175;95m";
        const BRIGHT: &str = "\x1b[1;38;2;240;235;225m";
        const TEXT: &str = "\x1b[38;2;200;195;185m";
        const DIM: &str = "\x1b[38;2;135;130;125m";
        const AMBER: &str = "\x1b[38;2;225;175;95m";
        const OFF: &str = "\x1b[0m";
        let border = "\x1b[38;2;160;155;145m";
        let box_w = width.saturating_sub(6).min(100);
        let inner_w = box_w.saturating_sub(2);
        let pad_row = |content: &str| -> String {
            let clipped = crate::tool_views::clip_ellipsis(content, inner_w);
            let pad = " ".repeat(inner_w.saturating_sub(crate::visible_width(&clipped)));
            format!("  {border}\u{2502}{OFF} {clipped}{pad} {border}\u{2502}{OFF}")
        };
        let title = match &self.form {
            Some(f) => format!(" {GOLD}Edit provider{OFF} {DIM}{}{OFF} ", f.original),
            None => format!(" {GOLD}Providers{OFF} {DIM}({}){OFF} ", self.profiles.len()),
        };
        let title = crate::tool_views::clip_ellipsis(&title, box_w.saturating_sub(2));
        let dashes = box_w.saturating_sub(crate::visible_width(&title) + 1);
        let mut lines = vec![(LineKind::System, format!("  {border}\u{256d}\u{2500}{title}{border}{}\u{256e}{OFF}", "\u{2500}".repeat(dashes)))];
        let mut push = |row: String| lines.push((LineKind::System, row));

        let hints: Vec<(&str, &str)> = match &self.form {
            Some(form) => {
                let label_w = 10;
                for (i, field) in fields(&form.draft).iter().enumerate() {
                    let current = i == form.row;
                    let ptr = if current { format!("{GOLD}\u{25b8}{OFF}") } else { " ".to_string() };
                    let typed = form.typing.as_deref().filter(|_| current);
                    let value = match (field, typed) {
                        (Field::Key, Some(t)) => format!(
                            "{GOLD}{}\u{2588}{OFF} {DIM}({}){OFF}",
                            crate::wizard::mask_api_key(t),
                            crate::plural(t.chars().count(), "character", "characters")
                        ),
                        (_, Some(t)) => format!("{GOLD}{t}\u{2588}{OFF}"),
                        (Field::Name, None) => form.draft.name.clone(),
                        (Field::Protocol, None) => format!("{}  {DIM}\u{2190}/\u{2192}{OFF}", form.draft.protocol.label()),
                        (Field::Address, None) => form.draft.url.clone(),
                        (Field::Key, None) => match &form.draft.api_key {
                            Some(key) => format!("{} {DIM}(saved){OFF}", crate::wizard::mask_api_key(key)),
                            None => {
                                let mut keyless = form.draft.clone();
                                keyless.api_key = None;
                                format!("{DIM}{}{OFF}", key_status(&keyless))
                            }
                        },
                        (Field::Model, None) if form.draft.model.is_empty() => format!("{DIM}the one the server has loaded{OFF}"),
                        (Field::Model, None) => form.draft.model.clone(),
                        (Field::Context, None) => match form.draft.context_window {
                            Some(n) => format!("{n} tokens"),
                            None => format!("{DIM}chosen for the model (32k unless loaded larger){OFF}"),
                        },
                        (Field::Save, _) => String::new(),
                    };
                    let row = match field {
                        Field::Save => {
                            push(pad_row(""));
                            let style = if current { GOLD } else { TEXT };
                            format!("{ptr} {style}[ Save ]{OFF}")
                        }
                        _ => {
                            let label = match field {
                                Field::Name => "Name",
                                Field::Protocol => "Protocol",
                                Field::Address => "Address",
                                Field::Key => "API key",
                                Field::Model => "Model",
                                Field::Context => "Context",
                                Field::Save => "",
                            };
                            let label_style = if current { BRIGHT } else { DIM };
                            format!("{ptr} {label_style}{label:<label_w$}{OFF} {TEXT}{value}{OFF}")
                        }
                    };
                    push(pad_row(&row));
                }
                match (fields(&form.draft)[form.row], &form.typing) {
                    (Field::Key, Some(_)) => vec![("Enter", "keep it"), ("empty", "use the environment"), ("Esc", "cancel")],
                    (Field::Context, Some(_)) => vec![("Enter", "keep it, e.g. 65536 or 64k"), ("empty", "automatic"), ("Esc", "cancel")],
                    (_, Some(_)) => vec![("Enter", "keep it"), ("Esc", "cancel")],
                    _ => vec![("\u{2191}/\u{2193}", "move"), ("Enter", "change"), ("Esc", "back without saving")],
                }
            }
            None => {
                let rows = self.profiles.len() + 1;
                let start = (self.selected + 1).saturating_sub(LIST_WINDOW).min(rows.saturating_sub(LIST_WINDOW));
                if start > 0 {
                    push(pad_row(&format!("  {DIM}\u{25b2} {start} more above{OFF}")));
                }
                for i in start..(start + LIST_WINDOW).min(rows) {
                    let current = i == self.selected;
                    let cursor = if current { format!("{GOLD}\u{203a}{OFF}") } else { " ".to_string() };
                    match self.profiles.get(i) {
                        Some(p) => {
                            let in_use = p.name == self.active;
                            let mark = if in_use { format!("{GOLD}\u{25cf}{OFF}") } else { format!("{DIM}\u{25cb}{OFF}") };
                            let name_style = if current { BRIGHT } else { TEXT };
                            let tag = if in_use { "in use \u{b7} " } else { "" };
                            push(pad_row(&format!("{cursor} {mark} {name_style}{}{OFF}  {DIM}{tag}{}{OFF}", p.name, key_status(p))));
                            push(pad_row(&format!("      {DIM}{}{OFF}", describe(p))));
                        }
                        None => {
                            let style = if current { BRIGHT } else { TEXT };
                            push(pad_row(&format!("{cursor} {style}+ Add a provider{OFF}")));
                        }
                    }
                }
                if start + LIST_WINDOW < rows {
                    push(pad_row(&format!("  {DIM}\u{25bc} {} more below{OFF}", rows - start - LIST_WINDOW)));
                }
                if self.confirm_delete {
                    vec![("Enter", "delete"), ("Esc", "keep it")]
                } else {
                    vec![("\u{2191}/\u{2193}", "move"), ("Enter", "switch"), ("e", "edit"), ("d", "delete"), ("a", "add"), ("Esc", "close")]
                }
            }
        };
        push(pad_row(""));
        if let Some(status) = &self.status {
            for row in crate::wrap_styled(&format!("{AMBER}{status}{OFF}"), inner_w.max(1)) {
                push(pad_row(&row));
            }
        }
        push(pad_row(&crate::key_hints(&hints, inner_w.saturating_sub(1))));
        lines.push((LineKind::System, format!("  {border}\u{2570}{}\u{256f}{OFF}", "\u{2500}".repeat(box_w))));
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flashagent_llm::{DiscoveredModel, ThinkingProfile};

    fn config() -> AppConfig {
        let mut cfg = AppConfig::default();
        let mut local = ProviderProfile::new("Local", ApiProtocol::OpenAi, "http://localhost:1234/v1");
        local.model = "qwen3-coder".into();
        cfg.save_provider(local);
        let mut cloud = ProviderProfile::new("Claude", ApiProtocol::Anthropic, "https://api.anthropic.com");
        cloud.api_key = Some("sk-ant-api03-secretsecret-9876".into());
        cfg.save_provider(cloud);
        cfg.activate("Local");
        cfg
    }

    fn model(id: &str, loaded: bool) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            display_name: None,
            is_loaded: loaded,
            context_length: Some(8192),
            max_context_length: None,
            thinking: ThinkingProfile::unreported(),
            supports_tools: true,
            supports_vision: false,
        }
    }

    fn discovery(models: Vec<DiscoveredModel>, active: Option<&str>) -> ServerDiscovery {
        let active_model = active.and_then(|id| models.iter().find(|m| m.id == id).cloned());
        ServerDiscovery { base_url: "http://x/v1".into(), models, active_model, kind: Default::default() }
    }

    fn text(lines: &[RenderLine]) -> String {
        lines.iter().map(|(_, l)| crate::strip_ansi(l)).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_saved_model_is_matched_exactly_before_by_part_of_its_name() {
        let disc = discovery(vec![model("gemini-2.5-flash", false), model("gemini-2.5-flash-lite", false)], None);
        assert_eq!(model_after_switch("gemini-2.5-flash-lite", Some(&disc)), "gemini-2.5-flash-lite");
        assert_eq!(model_after_switch("models/gemini-2.5-flash-lite", Some(&disc)), "gemini-2.5-flash-lite", "Google's prefix aside");
        assert_eq!(model_after_switch("gemini-2.5-flash-lite-preview-06-17", Some(&disc)), "gemini-2.5-flash-lite", "the closest id it holds");
        let disc = discovery(vec![model("qwen3-coder-30b-a3b", false), model("qwen3-coder-30b", false)], None);
        assert_eq!(model_after_switch("qwen3-coder", Some(&disc)), "qwen3-coder-30b", "the shortest id holding it");
        assert_eq!(model_after_switch("", Some(&disc)), "qwen3-coder-30b-a3b", "nothing saved: the first");
    }

    #[test]
    fn a_context_window_is_read_the_way_people_write_it() {
        assert_eq!(parse_context("65536"), Some(65_536));
        assert_eq!(parse_context("64k"), Some(65_536));
        assert_eq!(parse_context(" 128K "), Some(131_072));
        assert_eq!(parse_context("1m"), Some(1_048_576));
        assert_eq!(parse_context("32_768"), Some(32_768));
        assert_eq!(parse_context(""), None, "empty is automatic");
        assert_eq!(parse_context("0"), None);
        assert_eq!(parse_context("lots"), None);
    }

    #[test]
    fn only_a_server_that_lets_the_client_choose_offers_the_context_field() {
        let mut p = ProviderProfile::new("Ollama", ApiProtocol::Ollama, "http://localhost:11434");
        assert!(fields(&p).contains(&Field::Context));
        p.protocol = ApiProtocol::OpenAi;
        assert!(!fields(&p).contains(&Field::Context));
        assert_eq!(fields(&p).last(), Some(&Field::Save));
    }

    #[test]
    fn the_menu_marks_the_provider_in_use_and_shows_how_each_is_reached() {
        let menu = provider_menu(&config());
        assert_eq!(menu.selected_value().map(String::as_str), Some("Local"), "it opens on the one in use");
        let shown = text(&menu.render(100));
        assert!(shown.contains("\u{25cf} Local  (in use)"), "{shown}");
        assert!(shown.contains("\u{25cb} Claude"), "{shown}");
        assert!(shown.contains("OpenAI-compatible \u{b7} http://localhost:1234/v1 \u{b7} qwen3-coder"), "{shown}");
        assert!(shown.contains("Anthropic \u{b7} https://api.anthropic.com \u{b7} the loaded model"), "{shown}");
        assert!(shown.contains("(2 providers)"), "the actions are not counted as providers: {shown}");
        assert!(!shown.contains("secret"), "a key is never shown: {shown}");
        let values: Vec<&str> = menu.items.iter().map(|i| i.value.as_str()).collect();
        assert_eq!(values, ["Local", "Claude", ADD, MANAGE]);
    }

    #[test]
    fn a_url_given_for_one_run_heads_the_menu_as_the_one_in_use() {
        let mut cfg = config();
        cfg.use_url_for_this_run("http://gpu-box:8000/v1");
        let menu = provider_menu(&cfg);
        assert_eq!(menu.selected_value().map(String::as_str), Some(THIS_RUN));
        let shown = text(&menu.render(100));
        assert!(shown.contains("gpu-box:8000  (--url, this run only)"), "{shown}");
        assert!(!shown.contains("(in use)"), "{shown}");
    }

    #[test]
    fn a_saved_name_switches_at_once_and_anything_else_filters_the_menu() {
        let cfg = config();
        assert_eq!(provider_arg(&cfg, "claude"), ProviderArg::Switch("Claude".into()));
        assert_eq!(provider_arg(&cfg, " Local "), ProviderArg::Switch("Local".into()));
        assert_eq!(provider_arg(&cfg, "cla"), ProviderArg::Menu("cla".into()));
        assert_eq!(provider_arg(&cfg, ""), ProviderArg::Menu(String::new()));
    }

    #[test]
    fn after_a_switch_the_saved_model_wins_when_the_server_has_it() {
        let disc = discovery(vec![model("llama-3", true), model("qwen3-coder-30b", false)], Some("llama-3"));
        assert_eq!(model_after_switch("qwen3-coder-30b", Some(&disc)), "qwen3-coder-30b");
        assert_eq!(model_after_switch("qwen3-coder", Some(&disc)), "qwen3-coder-30b", "a partial name finds its full id");
        assert_eq!(model_after_switch("gone-model", Some(&disc)), "llama-3", "else the loaded one");
        assert_eq!(model_after_switch("", Some(&disc)), "llama-3");
        let unloaded = discovery(vec![model("first", false), model("second", false)], None);
        assert_eq!(model_after_switch("", Some(&unloaded)), "first", "else the first listed");
        assert_eq!(model_after_switch("kept", None), "kept", "no answer keeps what was saved");
        assert_eq!(model_after_switch("", Some(&discovery(Vec::new(), None))), "");
    }

    #[test]
    fn lm_studio_offers_only_its_loaded_models_when_it_has_any() {
        let mut disc = discovery(vec![model("loaded", true), model("on-disk", false)], Some("loaded"));
        assert_eq!(offered_models(&disc), ["loaded", "on-disk"]);
        disc.kind = flashagent_llm::thinking::ServerKind::LmStudio;
        assert_eq!(offered_models(&disc), ["loaded"]);
        disc.models[0].is_loaded = false;
        assert_eq!(offered_models(&disc), ["loaded", "on-disk"], "nothing loaded: all of them");
    }

    #[test]
    fn the_editor_changes_a_provider_and_never_shows_its_key() {
        let cfg = config();
        let mut view = ProvidersView::new(&cfg);
        assert_eq!(view.selected, 0, "it opens on the provider in use");
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        view.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        assert!(view.is_editing());
        let shown = text(&view.render(100));
        assert!(shown.contains("Edit provider Claude"), "{shown}");
        assert!(!shown.contains("secretsecret"), "{shown}");
        assert!(shown.contains("(saved)"), "{shown}");

        // Protocol with the arrows, the address typed, the key replaced, the model cleared.
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        view.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert!(text(&view.render(100)).contains("Google Gemini"));
        view.handle_key(KeyCode::Left, KeyModifiers::NONE);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        for _ in 0.."https://api.anthropic.com".len() {
            view.handle_key(KeyCode::Backspace, KeyModifiers::NONE);
        }
        view.handle_paste("claude-proxy:8080\n");
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        for c in "sk-new key".chars() {
            view.handle_key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        let typing = text(&view.render(100));
        assert!(!typing.contains("sk-newkey") && typing.contains("(9 characters)"), "{typing}");
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        let ProvidersAction::Save { original, profile } = view.handle_key(KeyCode::Enter, KeyModifiers::NONE) else {
            panic!("Save did not save");
        };
        assert_eq!(original, "Claude");
        assert_eq!(profile.protocol, ApiProtocol::Anthropic);
        assert_eq!(profile.url, "http://claude-proxy:8080", "an address without a scheme gets http://");
        assert_eq!(profile.api_key.as_deref(), Some("sk-newkey"));
    }

    #[test]
    fn an_empty_key_hands_the_provider_back_to_its_environment_variable() {
        let mut view = ProvidersView::new(&config());
        view.selected = 1;
        view.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        for _ in 0..3 {
            view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        }
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        let ProvidersAction::Save { profile, .. } = view.handle_key(KeyCode::Enter, KeyModifiers::NONE) else { panic!() };
        assert_eq!(profile.api_key, None);
        // Esc leaves the editor with nothing changed.
        view.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
        view.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!view.is_editing());
    }

    #[test]
    fn the_provider_in_use_is_not_deleted_and_another_is_only_after_a_yes() {
        let mut view = ProvidersView::new(&config());
        assert_eq!(view.handle_key(KeyCode::Char('d'), KeyModifiers::NONE), ProvidersAction::None);
        assert!(view.status.as_deref().is_some_and(|s| s.contains("in use")));
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        view.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(view.handle_key(KeyCode::Char('x'), KeyModifiers::NONE), ProvidersAction::None, "any other key keeps it");
        view.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::NONE), ProvidersAction::Delete("Claude".into()));
    }

    #[test]
    fn enter_switches_and_the_last_row_adds() {
        let mut view = ProvidersView::new(&config());
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::NONE), ProvidersAction::Switch("Claude".into()));
        view.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(view.handle_key(KeyCode::Enter, KeyModifiers::NONE), ProvidersAction::Add);
        assert_eq!(view.handle_key(KeyCode::Char('a'), KeyModifiers::NONE), ProvidersAction::Add);
        assert_eq!(view.handle_key(KeyCode::Esc, KeyModifiers::NONE), ProvidersAction::Close);
    }

    #[test]
    fn every_row_fits_the_width_and_the_border_closes() {
        let mut cfg = config();
        for i in 0..9 {
            cfg.save_provider(ProviderProfile::new(format!("Box {i} with a long name"), ApiProtocol::OpenAi, format!("http://192.168.1.{i}:8000/v1")));
        }
        let mut view = ProvidersView::new(&cfg);
        view.status = Some("A status long enough to wrap when the window is narrow, which it must do".into());
        for width in [30usize, 44, 60, 80, 120] {
            for editing in [false, true] {
                if editing {
                    view.handle_key(KeyCode::Char('e'), KeyModifiers::NONE);
                }
                let rows = view.render(width);
                let first = crate::visible_width(&rows[0].1);
                for (_, row) in &rows {
                    assert!(crate::visible_width(row) <= width, "{width}: {}", crate::strip_ansi(row));
                    assert_eq!(crate::visible_width(row), first, "{width}: {}", crate::strip_ansi(row));
                }
                if editing {
                    view.handle_key(KeyCode::Esc, KeyModifiers::NONE);
                }
            }
        }
        view.selected = view.profiles.len();
        let shown = text(&view.render(100));
        assert!(shown.contains("more above") && shown.contains("+ Add a provider"), "{shown}");
    }
}
