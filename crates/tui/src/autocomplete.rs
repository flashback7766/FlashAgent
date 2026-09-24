//! Popup for slash commands and skills, shown when input starts with `/`.

use std::path::Path;
use crate::{visible_width, LineKind, RenderLine};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutocompleteCategory {
    Command,
    Skill,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompleteItem {
    /// E.g. "/help", "/expand", "/skill:rust-core".
    pub trigger: String,
    pub description: String,
    /// Only a skill is labeled in the popup.
    pub category: AutocompleteCategory,
}

impl AutocompleteItem {
    pub fn new(
        trigger: impl Into<String>,
        description: impl Into<String>,
        category: AutocompleteCategory,
    ) -> Self {
        Self {
            trigger: trigger.into(),
            description: description.into(),
            category,
        }
    }
}

/// Levenshtein by characters, for "did you mean".
pub fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let swap = prev[j] + usize::from(ca != *cb);
            cur[j + 1] = swap.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

pub fn builtin_commands() -> Vec<AutocompleteItem> {
    vec![
        AutocompleteItem::new("/help", "Show command reference and keybindings", AutocompleteCategory::Command),
        AutocompleteItem::new("/settings", "Open interactive settings tab (or press Tab on empty prompt)", AutocompleteCategory::Command),
        AutocompleteItem::new("/sampling", "Sampling parameters, for those who tune them", AutocompleteCategory::Command),
        AutocompleteItem::new("/memory", "See what FlashAgent remembers; forget or correct a fact", AutocompleteCategory::Command),
        AutocompleteItem::new("/whatsnew", "Show what changed in the latest releases", AutocompleteCategory::Command),
        AutocompleteItem::new("/context", "Show detailed context window token breakdown", AutocompleteCategory::Command),
        AutocompleteItem::new("/clear", "Clear terminal screen and conversation scrollback", AutocompleteCategory::Command),
        AutocompleteItem::new("/effort", "Select thinking effort preset (off, low, medium, high)", AutocompleteCategory::Command),
        AutocompleteItem::new("/model", "Open model selection menu or switch model", AutocompleteCategory::Command),
        AutocompleteItem::new("/provider", "Switch to another saved provider, local or cloud; add or edit them", AutocompleteCategory::Command),
        AutocompleteItem::new("/verbose", "Toggle verbose mode for thoughts and tool calls (all, last, off)", AutocompleteCategory::Command),
        AutocompleteItem::new("/mode", "Cycle permission mode (Planning, Manual, Accept Edits, Accept All)", AutocompleteCategory::Command),
        AutocompleteItem::new("/goal", "Autonomous run: /goal <task> (limits in Settings → Goal)", AutocompleteCategory::Command),
        AutocompleteItem::new("/resume", "Pick a saved session from this folder and continue it", AutocompleteCategory::Command),
        AutocompleteItem::new("/mcp", "List and manage Model Context Protocol servers", AutocompleteCategory::Command),
        AutocompleteItem::new("/tasks", "Background commands: see their output, stop one", AutocompleteCategory::Command),
        AutocompleteItem::new("/compact", "Compact conversation context (optional: /compact <focus instructions>)", AutocompleteCategory::Command),
        AutocompleteItem::new("/regenerate", "Regenerate the last assistant response from scratch (or press Ctrl+R)", AutocompleteCategory::Command),
        AutocompleteItem::new("/update", "Check and apply FlashAgent updates in-place", AutocompleteCategory::Command),
        AutocompleteItem::new("/channel", "Switch release channel: /channel <stable|beta>", AutocompleteCategory::Command),
        AutocompleteItem::new("/diff", "Preview unstaged git changes and diff stat", AutocompleteCategory::Command),
        AutocompleteItem::new("/commit", "Review changes and commit with /commit <message>", AutocompleteCategory::Command),
        AutocompleteItem::new("/rewind", "Take turns back: files and conversation return to before a turn", AutocompleteCategory::Command),
        AutocompleteItem::new("/editor", "Open external editor (nano/vim/code) to craft prompt", AutocompleteCategory::Command),
        AutocompleteItem::new("/export", "Export chat session to markdown, HTML, or JSONL", AutocompleteCategory::Command),
        AutocompleteItem::new("/exit", "Save the session and quit", AutocompleteCategory::Command),
        AutocompleteItem::new("/uninstall", "Close FlashAgent and remove it; asks what data to delete", AutocompleteCategory::Command),
        AutocompleteItem::new("/skills", "List skills from .agents/skills and ~/.flashagent/skills", AutocompleteCategory::Command),
    ]
}

/// The saved providers, `(name, how it is reached)`, for `/provider <name>`.
/// Set by the app whenever its config is saved; completion reads no files.
static PROVIDERS: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());

pub fn set_provider_names(providers: Vec<(String, String)>) {
    *PROVIDERS.lock().unwrap_or_else(|e| e.into_inner()) = providers;
}

pub fn sub_commands(input: &str) -> Option<Vec<AutocompleteItem>> {
    let lower = input.to_lowercase();
    if lower.starts_with("/provider ") {
        let providers = PROVIDERS.lock().unwrap_or_else(|e| e.into_inner());
        return Some(
            providers
                .iter()
                .map(|(name, reached)| AutocompleteItem::new(format!("/provider {name}"), reached.clone(), AutocompleteCategory::Command))
                .collect(),
        );
    }
    if lower.starts_with("/expand ") || lower == "/expand" || lower.starts_with("/verbose ") || lower == "/verbose" {
        Some(vec![
            AutocompleteItem::new("/verbose all", "Expand both thoughts and tool calls permanently", AutocompleteCategory::Command),
            AutocompleteItem::new("/verbose last", "Expand thoughts and tool calls for latest turn", AutocompleteCategory::Command),
            AutocompleteItem::new("/verbose off", "Collapse thoughts and tool calls to concise summary", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/memory ") || lower == "/memory" {
        Some(vec![
            AutocompleteItem::new("/memory", "List what FlashAgent remembers; forget or correct a fact", AutocompleteCategory::Command),
            AutocompleteItem::new("/memory summary", "An overview of everything remembered, by topic", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/effort ") || lower == "/effort" {
        Some(vec![
            AutocompleteItem::new("/effort default", "Use API server default preset", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort off", "Disable model internal reasoning", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort low", "Fast response, low reasoning budget", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort medium", "Balanced reasoning depth and speed", AutocompleteCategory::Command),
            AutocompleteItem::new("/effort high", "Full reasoning exploration", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/mode ") || lower == "/mode" {
        Some(vec![
            AutocompleteItem::new("/mode manual", "Require confirmation for file writes and shell", AutocompleteCategory::Command),
            AutocompleteItem::new("/mode edits", "Auto-approve file edits, ask on shell commands", AutocompleteCategory::Command),
            AutocompleteItem::new("/mode planning", "Read-only planning mode (modifications blocked)", AutocompleteCategory::Command),
            AutocompleteItem::new("/mode bypass", "Bypass all approval gates (full autonomy)", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/mcp ") || lower == "/mcp" {
        Some(vec![
            AutocompleteItem::new("/mcp list", "List configured and connected MCP servers", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp market", "Browse vetted MCP marketplace extensions", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp test", "Test connection and discover tools for an MCP server", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp add", "Add and scaffold a curated MCP extension from marketplace", AutocompleteCategory::Command),
            AutocompleteItem::new("/mcp reload", "Reload MCP configurations and restart active servers", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/channel ") || lower == "/channel" {
        Some(vec![
            AutocompleteItem::new("/channel beta", "Switch to Beta channel (latest features & pre-releases)", AutocompleteCategory::Command),
            AutocompleteItem::new("/channel stable", "Switch to Stable channel (official releases)", AutocompleteCategory::Command),
        ])
    } else if lower.starts_with("/compact ") || lower == "/compact" {
        Some(vec![
            AutocompleteItem::new("/compact", "Auto-compact prior context while keeping current state", AutocompleteCategory::Command),
            AutocompleteItem::new("/compact keep code details", "Compact context prioritizing code and architecture details", AutocompleteCategory::Command),
            AutocompleteItem::new("/compact keep task goals", "Compact context prioritizing remaining tasks and roadmap", AutocompleteCategory::Command),
        ])
    } else {
        None
    }
}

/// `load_skills`, read again at most every two seconds: the popup asks every frame.
fn cached_skills(cwd: &Path) -> Vec<AutocompleteItem> {
    type Cache = Option<(std::time::Instant, std::path::PathBuf, Vec<AutocompleteItem>)>;
    static CACHE: std::sync::Mutex<Cache> = std::sync::Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, dir, items)) = cache.as_ref() {
        if dir == cwd && at.elapsed() < std::time::Duration::from_secs(2) {
            return items.clone();
        }
    }
    let items = load_skills(cwd);
    *cache = Some((std::time::Instant::now(), cwd.to_path_buf(), items.clone()));
    items
}

/// Enter on such an item puts it in the prompt to be finished instead of
/// running it bare.
/// Short and second names the command handler takes (`/m` is `/model`); a
/// line that already is one runs as typed instead of becoming the popup's
/// first item.
pub const COMMAND_ALIASES: &[&str] = &[
    "/?", "/config", "/changelog", "/memories", "/retry", "/thinking", "/t", "/models", "/m", "/params", "/expand",
    "/think", "/o", "/quit", "/q", "/providers",
];

/// A whole command or alias, as typed.
pub fn is_exact_command(input: &str) -> bool {
    let typed = input.trim().to_lowercase();
    COMMAND_ALIASES.contains(&typed.as_str()) || builtin_commands().iter().any(|c| c.trigger.to_lowercase() == typed)
}

pub fn needs_argument(trigger: &str) -> bool {
    matches!(trigger, "/goal" | "/commit" | "/channel")
}

/// From `.agents/skills/*.md` and `~/.flashagent/skills/*.md`.
pub fn load_skills(cwd: &Path) -> Vec<AutocompleteItem> {
    let mut skills = Vec::new();
    let mut scanned_dirs = vec![cwd.join(".agents").join("skills")];
    if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
        scanned_dirs.push(std::path::PathBuf::from(home).join(".flashagent").join("skills"));
    }

    for dir in scanned_dirs {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };

            let desc = if let Ok(content) = std::fs::read_to_string(&path) {
                let mut found_desc = None;
                // `description:` is the frontmatter key skill files actually use; reading
                // only `Trigger:` ignored it.
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(rest) = trimmed
                        .strip_prefix("Trigger:")
                        .or_else(|| trimmed.strip_prefix("description:"))
                        .or_else(|| trimmed.strip_prefix("Description:"))
                    {
                        let text = rest.trim().trim_matches('"').trim();
                        if !text.is_empty() {
                            found_desc = Some(text.to_string());
                            break;
                        }
                    }
                }
                found_desc.unwrap_or_else(|| format!("Skill defined in {}", path.file_name().unwrap_or_default().to_string_lossy()))
            } else {
                format!("Skill {stem}")
            };

            // Listed once, as /<name>; /skill:<name> still runs it, and is what is
            // listed when a built-in command already has the short name.
            let short = format!("/{stem}");
            let trigger = if builtin_commands().iter().any(|c| c.trigger == short) { format!("/skill:{stem}") } else { short };
            if !skills.iter().any(|s: &AutocompleteItem| s.trigger == trigger) {
                skills.push(AutocompleteItem::new(trigger, desc, AutocompleteCategory::Skill));
            }
        }
    }

    skills
}

pub fn find_matches(input: &str, cwd: &Path) -> Vec<AutocompleteItem> {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return Vec::new();
    }

    // After a space the command is chosen: its sub-commands, or nothing while its
    // argument is typed ("/goal fix the parser" is not a search).
    if input.trim_start().contains(' ') {
        let lower = input.trim_start().to_lowercase();
        return sub_commands(&lower)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.trigger.to_lowercase().starts_with(lower.trim_end()) && s.trigger.to_lowercase() != lower.trim_end())
            .collect();
    }

    let mut pool = builtin_commands();
    pool.extend(cached_skills(cwd));

    let query = trimmed.to_lowercase();
    let sub_query = query.trim_start_matches('/');

    let mut prefix_matches = Vec::new();
    let mut substr_matches = Vec::new();
    let mut desc_matches = Vec::new();

    for item in pool {
        let trig_lower = item.trigger.to_lowercase();
        let desc_lower = item.description.to_lowercase();

        if trig_lower.starts_with(&query) {
            prefix_matches.push(item);
        } else if !sub_query.is_empty() && trig_lower.contains(sub_query) {
            substr_matches.push(item);
        } else if sub_query.chars().count() >= 3 && desc_lower.contains(sub_query) {
            // Shorter, and every description with a "c" in it matched "/c".
            desc_matches.push(item);
        }
    }

    // The command typed in full comes first: `/mode` is not `/model`.
    if let Some(exact) = prefix_matches.iter().position(|item| item.trigger.to_lowercase() == query) {
        let item = prefix_matches.remove(exact);
        prefix_matches.insert(0, item);
    }
    prefix_matches.extend(substr_matches);
    prefix_matches.extend(desc_matches);
    prefix_matches
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutocompletePopup {
    pub items: Vec<AutocompleteItem>,
    pub selected: usize,
}

impl AutocompletePopup {
    /// `None` unless input starts with `/` and something matches.
    pub fn for_input(input: &str, cwd: &Path, prev_selected: usize) -> Option<Self> {
        let items = find_matches(input, cwd);
        if items.is_empty() {
            None
        } else {
            let selected = if items.is_empty() {
                0
            } else {
                prev_selected.min(items.len() - 1)
            };
            Some(Self { items, selected })
        }
    }

    /// Wraps around.
    pub fn next(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + 1) % self.items.len();
        }
    }

    /// Wraps around.
    pub fn prev(&mut self) {
        if !self.items.is_empty() {
            self.selected = (self.selected + self.items.len() - 1) % self.items.len();
        }
    }

    pub fn current(&self) -> Option<&AutocompleteItem> {
        self.items.get(self.selected)
    }

    pub fn complete_input(&self, current: &str) -> String {
        if let Some(item) = self.current() {
            if current == item.trigger || current == format!("{} ", item.trigger) {
                // Already completed: Tab cycles to the next item.
                let next_idx = (self.selected + 1) % self.items.len();
                format!("{} ", self.items[next_idx].trigger)
            } else {
                format!("{} ", item.trigger)
            }
        } else {
            current.to_string()
        }
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let mut lines = Vec::new();
        let inner_w = width.saturating_sub(2);
        let border_color = "\x1b[38;2;100;95;90m";
        let reset = "\x1b[0m";

        let title = " Commands ";
        let top_dashes = inner_w.saturating_sub(title.chars().count() + 1);
        lines.push((
            LineKind::System,
            format!("{border_color}╭─\x1b[38;2;180;175;165m{title}{border_color}{}╮{reset}", "─".repeat(top_dashes)),
        ));

        const MAX_VISIBLE: usize = 5;
        let total = self.items.len();
        let start_idx = if total <= MAX_VISIBLE || self.selected < MAX_VISIBLE / 2 {
            0
        } else if self.selected + (MAX_VISIBLE / 2) >= total {
            total - MAX_VISIBLE
        } else {
            self.selected - (MAX_VISIBLE / 2)
        };
        let end_idx = (start_idx + MAX_VISIBLE).min(total);

        for (i, item) in self.items[start_idx..end_idx].iter().enumerate() {
            let actual_idx = start_idx + i;
            let is_sel = actual_idx == self.selected;

            let pointer = if is_sel {
                "\x1b[1;38;2;225;175;95m▸\x1b[0m"
            } else {
                " "
            };

            // Every row is a command; only a skill says what it is.
            let badge_text = match item.category {
                AutocompleteCategory::Command => "",
                AutocompleteCategory::Skill => " skill",
            };

            let trigger_w = item.trigger.chars().count().max(18);
            let cmd_styled = if is_sel {
                format!("\x1b[1;38;2;240;235;225m{:<trigger_w$}\x1b[0m", item.trigger)
            } else {
                format!("\x1b[38;2;180;175;165m{:<trigger_w$}\x1b[0m", item.trigger)
            };

            // Pointer, trigger, badge, the spaces between them and a right margin.
            let badge_w = badge_text.chars().count();
            let fixed_w = 1 + 1 + 1 + trigger_w + 1 + badge_w + 1;
            let desc_budget = inner_w.saturating_sub(fixed_w);

            let desc_clipped = crate::tool_views::clip_ellipsis(&item.description, desc_budget);
            let desc_styled = if is_sel {
                format!("\x1b[38;2;215;210;200m{desc_clipped}\x1b[0m")
            } else {
                format!("\x1b[38;2;135;130;125m{desc_clipped}\x1b[0m")
            };

            let row_raw = crate::tool_views::clip_ellipsis(
                &format!(" {pointer} {cmd_styled} {desc_styled}"),
                inner_w.saturating_sub(badge_w + 1),
            );
            let vis_len = visible_width(&row_raw) + badge_w + 1;
            let pad = " ".repeat(inner_w.saturating_sub(vis_len));
            lines.push((
                LineKind::System,
                format!("{border_color}│{reset}{row_raw}{pad}\x1b[38;2;125;121;115m{badge_text}{reset} {border_color}│{reset}"),
            ));
        }

        let bot_hint = if total > MAX_VISIBLE {
            format!(" {} of {} ", self.selected + 1, total)
        } else {
            String::new()
        };
        let bot_dashes = inner_w.saturating_sub(bot_hint.chars().count());
        lines.push((
            LineKind::System,
            format!("{border_color}╰{}\x1b[38;2;130;125;120m{bot_hint}{border_color}╯{reset}", "─".repeat(bot_dashes)),
        ));

        lines
    }
}

#[cfg(test)]
mod skill_tests {
    #[test]
    fn a_skill_is_listed_by_its_own_description() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join(".agents/skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(
            skills.join("greet.md"),
            "---\nname: greet\ndescription: Say hello politely\n---\n\nBody.\n",
        )
        .unwrap();
        let items = super::load_skills(dir.path());
        let greet = items.iter().find(|i| i.trigger.contains("greet")).expect("skill found");
        assert_eq!(greet.description, "Say hello politely");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_saved_providers_complete_after_the_command() {
        set_provider_names(vec![("LM Studio".into(), "OpenAI-compatible".into()), ("Anthropic".into(), "Anthropic".into())]);
        let found: Vec<String> = find_matches("/provider an", Path::new(".")).into_iter().map(|i| i.trigger).collect();
        assert_eq!(found, vec!["/provider Anthropic"], "matched however it is written");
        assert_eq!(find_matches("/provider ", Path::new(".")).len(), 2);
        assert!(find_matches("/provider Anthropic", Path::new(".")).is_empty(), "typed in full: nothing left to offer");
        set_provider_names(Vec::new());
    }

    #[test]
    fn a_command_typed_in_full_is_not_swapped_for_a_longer_one() {
        let found = find_matches("/mode", std::path::Path::new("."));
        assert_eq!(found.first().map(|i| i.trigger.as_str()), Some("/mode"));
        for typed in ["/mode", "/m", "/t", "/o", "/think", "/q"] {
            assert!(is_exact_command(typed), "{typed}");
        }
        assert!(!is_exact_command("/he"));
    }

    #[test]
    fn test_builtin_commands_exist() {
        let cmds = builtin_commands();
        assert!(cmds.iter().any(|c| c.trigger == "/help"));
        assert!(cmds.iter().any(|c| c.trigger == "/verbose"));
        // Aliases still run, but are not listed twice.
        assert!(!cmds.iter().any(|c| c.trigger == "/expand" || c.trigger == "/retry"));
        assert!(cmds.iter().any(|c| c.trigger == "/effort"));
    }

    #[test]
    fn test_find_matches_prefix_filtering() {
        let temp = std::env::temp_dir();
        let matches = find_matches("/ex", &temp);
        let triggers: Vec<&str> = matches.iter().map(|m| m.trigger.as_str()).collect();
        // Names that start with it first, then names that contain it.
        assert_eq!(&triggers[..2], ["/export", "/exit"]);
    }

    #[test]
    fn test_autocomplete_popup_navigation() {
        let temp = std::env::temp_dir();
        let mut popup = AutocompletePopup::for_input("/e", &temp, 0).expect("should find matches for /e");
        assert!(popup.items.len() >= 2);
        let first = popup.current().unwrap().trigger.clone();
        popup.next();
        let second = popup.current().unwrap().trigger.clone();
        assert_ne!(first, second);
        popup.prev();
        assert_eq!(popup.current().unwrap().trigger, first);
    }

    #[test]
    fn test_autocomplete_render_lines() {
        let temp = std::env::temp_dir();
        let popup = AutocompletePopup::for_input("/", &temp, 0).expect("should find matches for /");
        let rendered = popup.render(80);
        assert!(rendered.len() >= 3); // top border + items + bottom border
        assert!(rendered[0].1.contains("Commands"));
    }

    #[test]
    fn only_a_skill_is_labeled_and_every_row_is_as_wide_as_the_frame() {
        let popup = AutocompletePopup {
            items: vec![
                AutocompleteItem::new("/help", "Show command reference and keybindings", AutocompleteCategory::Command),
                AutocompleteItem::new("/greet", "Say hello politely to whoever is there", AutocompleteCategory::Skill),
                AutocompleteItem::new("/compact keep code details", "Compact context prioritizing code", AutocompleteCategory::Command),
            ],
            selected: 0,
        };
        for width in [40usize, 60, 100] {
            let rows: Vec<String> = popup.render(width).iter().map(|(_, l)| crate::strip_ansi(l)).collect();
            assert!(rows.iter().all(|r| visible_width(r) == width), "{width}: {rows:#?}");
            assert!(!rows.iter().any(|r| r.contains("[cmd]")), "{rows:#?}");
            assert!(rows[2].trim_end_matches('│').trim_end().ends_with("skill"), "{:?}", rows[2]);
        }
    }

    #[test]
    fn test_tab_completion_formatting() {
        let temp = std::env::temp_dir();
        let popup = AutocompletePopup::for_input("/h", &temp, 0).expect("matches");
        let completed = popup.complete_input("/h");
        assert_eq!(completed, "/help ");
    }

    #[test]
    fn a_short_query_matches_names_not_descriptions() {
        let temp = std::env::temp_dir();
        let matches = find_matches("/c", &temp);
        assert!(matches.iter().all(|m| m.trigger.contains('c')), "{:?}", matches.iter().map(|m| &m.trigger).collect::<Vec<_>>());
    }

    #[test]
    fn typing_an_argument_hides_the_popup_and_a_sub_command_shows_its_own() {
        let temp = std::env::temp_dir();
        assert!(find_matches("/goal fix the parser", &temp).is_empty());
        let modes: Vec<String> = find_matches("/mode ", &temp).into_iter().map(|m| m.trigger).collect();
        assert!(modes.contains(&"/mode manual".to_string()), "{modes:?}");
        let narrowed: Vec<String> = find_matches("/mode pl", &temp).into_iter().map(|m| m.trigger).collect();
        assert_eq!(narrowed, ["/mode planning"]);
        // Written out in full: nothing left to suggest.
        assert!(find_matches("/mode planning", &temp).is_empty());
    }

    #[test]
    fn a_skill_is_listed_once() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join(".agents/skills");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("greet.md"), "description: Say hello\n").unwrap();
        std::fs::write(skills.join("help.md"), "description: Clashes with /help\n").unwrap();
        let items = load_skills(dir.path());
        let triggers: Vec<&str> = items.iter().map(|i| i.trigger.as_str()).collect();
        assert!(triggers.contains(&"/greet") && !triggers.contains(&"/skill:greet"), "{triggers:?}");
        assert!(triggers.contains(&"/skill:help") && !triggers.contains(&"/help"), "{triggers:?}");
    }
}
