use super::*;

/// Titled with the provider, so a switch shows whose models these are.
pub(crate) fn build_model_menu(source: &BackendSource, provider: &str) -> Option<SelectMenu<String>> {
    let disc = source.discovery()?;
    if disc.models.is_empty() {
        return None;
    }
    let is_lm_studio = disc.kind == flashagent_llm::thinking::ServerKind::LmStudio;
    let has_loaded = disc.models.iter().any(|m| m.is_loaded);
    let models: Vec<_> = if is_lm_studio && has_loaded {
        disc.models.iter().filter(|m| m.is_loaded).collect()
    } else {
        disc.models.iter().collect()
    };

    let mut items = Vec::new();
    for m in models {
        let summary = m.capabilities_summary();
        let load_tag = if m.is_loaded { "● loaded" } else { "○ available" };
        let desc = format!("{load_tag} · {summary}");
        items.push(SelectItem::with_description(m.id.clone(), desc, m.id.clone()));
    }
    Some(SelectMenu::new(format!("Select model \u{b7} {provider}"), items).with_noun("models"))
}

pub(crate) fn build_effort_menu(
    source: &BackendSource,
    memory: &flashagent_core::EffortMemory,
    model: &str,
) -> SelectMenu<String> {
    let mut items = Vec::new();
    // Auto changes behind the user's back, so it says what it decided. An empty
    // profile means "cannot reason" or "not discovered yet"; only discovery can
    // tell them apart, and only the server saying so counts as "cannot".
    let can_think = match source.profile() {
        Some(p) => p.supported || p.is_unreported(),
        None => source
            .discovery()
            .and_then(|d| d.models.iter().find(|m| m.id == model).map(|m| m.thinking.supported || m.thinking.is_unreported()))
            .unwrap_or(true),
    };
    let auto_desc = if !can_think {
        // The setting is kept, but it is not in force.
        "Auto (this model does not reason; kept for the next one)".to_string()
    } else {
        match memory.explain(model) {
            Some(learned) => format!("Auto (per turn; learned {learned})"),
            None => "Auto (dynamically adjusts thinking per turn)".to_string(),
        }
    };
    items.push(SelectItem::with_description(
        "auto",
        &auto_desc,
        "auto".to_string(),
    ));
    if let Some(prof) = source.profile() {
        if prof.supported && !prof.presets.is_empty() {
            for p in &prof.presets {
                let desc = match p.to_lowercase().as_str() {
                    "off" | "disabled" | "none" | "false" | "0" => "Disable internal reasoning",
                    "low" | "minimal" | "min" | "fast" => "Fast response, low thinking budget",
                    "medium" | "standard" => "Balanced reasoning depth and speed",
                    "high" | "deep" => "Full reasoning exploration",
                    "xhigh" | "extra-high" => "Maximum thinking budget for difficult tasks",
                    "on" | "enabled" | "true" | "1" => "Enable internal reasoning",
                    _ => "API model preset",
                };
                items.push(SelectItem::with_description(p.clone(), desc, p.clone()));
            }
        } else if prof.is_unreported() {
            // The server lists no settings: off is the only honest option besides default.
            items.push(SelectItem::with_description("off", "Disable reasoning (the server lists no other settings)", "off".to_string()));
        } else if !prof.supported {
            items.push(SelectItem::with_description("off", "Reasoning unsupported by this endpoint", "off".to_string()));
        }
    }
    if items.len() == 1 {
        items.push(SelectItem::with_description("off", "Disable reasoning", "off".to_string()));
        items.push(SelectItem::with_description("low", "Minimal reasoning budget", "low".to_string()));
        items.push(SelectItem::with_description("medium", "Balanced reasoning depth", "medium".to_string()));
        items.push(SelectItem::with_description("high", "Deep reasoning exploration", "high".to_string()));
    }
    SelectMenu::new("Thinking effort", items).with_noun("options")
}

pub(crate) fn thinking_summary_str(source: &BackendSource, current_effort: &str) -> String {
    if let Some(ref p) = source.profile() {
        if p.supported && !p.presets.is_empty() {
            format!("{} [{}]", current_effort, p.presets.join(", "))
        } else if !p.supported && !p.is_unreported() {
            "disabled (unsupported)".to_string()
        } else {
            current_effort.to_string()
        }
    } else {
        current_effort.to_string()
    }
}

/// The live session state (mode, effort, model), not the stored defaults.
pub(crate) fn settings_for_runtime(
    config: &AppConfig,
    mode: PermissionMode,
    effort: &str,
    model: &str,
    models: &[String],
    context_capacity: usize,
) -> SettingsView {
    let mut cfg = config.clone();
    cfg.permission_mode = mode;
    cfg.thinking_effort = effort.to_string();
    if !model.is_empty() {
        cfg.active_profile_mut().model = model.to_string();
    }
    let mut view = SettingsView::new(cfg, models.to_vec());
    view.context_capacity = context_capacity;
    view
}

/// Live mode and effort become defaults only if changed here: opening Settings
/// mid-session (or during /goal's Accept All) must not change the next launch.
pub(crate) fn persisted_from_view(view: &AppConfig, persisted: &AppConfig, shown_mode: PermissionMode, shown_effort: &str) -> AppConfig {
    let mut cfg = view.clone();
    if cfg.permission_mode == shown_mode {
        cfg.permission_mode = persisted.permission_mode;
    }
    if cfg.thinking_effort == shown_effort {
        cfg.thinking_effort = persisted.thinking_effort.clone();
    }
    cfg
}

/// Labelled relative to the project, so the card does not need the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RewindFileRow {
    pub display: String,
    pub added: usize,
    pub removed: usize,
    pub will_delete: bool,
    pub too_large: bool,
}

impl From<(&flashagent_core::SnapshotStore, flashagent_core::FilePreview)> for RewindFileRow {
    fn from((store, f): (&flashagent_core::SnapshotStore, flashagent_core::FilePreview)) -> Self {
        RewindFileRow {
            display: store.display_path(&f.path),
            added: f.added,
            removed: f.removed,
            will_delete: f.will_delete,
            too_large: f.too_large,
        }
    }
}

/// Taking turns back never happens on a keystroke alone.
pub(crate) struct RewindConfirm {
    pub target: flashagent_core::Rewindable,
    pub prompt_label: String,
    pub files: Vec<RewindFileRow>,
    /// `true` selects "Yes, rewind".
    pub confirm: bool,
}

impl RewindConfirm {
    pub fn toggle(&mut self) {
        self.confirm = !self.confirm;
    }

    pub fn render(&self, width: usize) -> Vec<RenderLine> {
        let card_w = width.clamp(36, 90);
        let inner_w = card_w.saturating_sub(2);
        let border_color = "\x1b[38;2;95;90;85m";
        let reset = "\x1b[0m";

        let pad_row = |content: &str| -> String {
            let vis = visible_width(content);
            let clipped = if vis > inner_w { clip_ansi(content, inner_w) } else { content.to_string() };
            let clipped_vis = visible_width(&clipped);
            let pad = inner_w.saturating_sub(clipped_vis);
            format!("{border_color}│{reset}{clipped}{}{border_color}│{reset}", " ".repeat(pad))
        };

        let mut lines = Vec::new();
        let title_styled = " \x1b[1;38;2;225;175;95mConfirm Rewind\x1b[0m ";
        let title_vis = visible_width(title_styled);
        let border_dashes = inner_w.saturating_sub(title_vis);
        lines.push((
            LineKind::System,
            format!("{border_color}╭─{title_styled}{}╮{reset}", "─".repeat(border_dashes.saturating_sub(1))),
        ));

        lines.push((
            LineKind::System,
            pad_row(&format!(
                "  \x1b[38;2;160;155;145mRewinding to before: \"{}\"\x1b[0m",
                self.prompt_label
            )),
        ));
        lines.push((LineKind::System, pad_row("")));

        if self.files.is_empty() {
            lines.push((
                LineKind::System,
                pad_row("  \x1b[38;2;135;130;125mThis will not change any files.\x1b[0m"),
            ));
        } else {
            for f in &self.files {
                let stat = if f.too_large {
                    "\x1b[38;2;135;130;125m(no copy was kept; left as it is)\x1b[0m".to_string()
                } else if f.will_delete {
                    format!("\x1b[38;2;230;120;120m-{} (removed)\x1b[0m", f.removed)
                } else {
                    format!("\x1b[38;2;145;205;140m+{}\x1b[0m \x1b[38;2;230;120;120m-{}\x1b[0m", f.added, f.removed)
                };
                lines.push((
                    LineKind::System,
                    pad_row(&format!("  \x1b[38;2;225;230;240m{}\x1b[0m  {stat}", f.display)),
                ));
            }
        }
        lines.push((LineKind::System, pad_row("")));

        for (label, is_yes) in [("Yes, rewind", true), ("No, cancel", false)] {
            let is_cur = self.confirm == is_yes;
            let (cursor, label_styled) = if is_cur {
                (" \x1b[1;38;2;225;175;95m›\x1b[0m", format!("\x1b[1;38;2;245;240;232m{label}\x1b[0m"))
            } else {
                ("  ", format!("\x1b[38;2;160;155;145m{label}\x1b[0m"))
            };
            lines.push((LineKind::System, pad_row(&format!("{cursor} {label_styled}"))));
        }

        lines.push((LineKind::System, format!("{border_color}╰{}╯{reset}", "─".repeat(inner_w))));
        lines.push((
            LineKind::System,
            "  \x1b[38;2;135;130;125m↑/↓ — choose · enter — confirm · esc — cancel\x1b[0m".to_string(),
        ));
        lines
    }
}

#[cfg(test)]
mod rewind_confirm_tests {
    use super::*;

    fn row(display: &str, added: usize, removed: usize) -> RewindFileRow {
        RewindFileRow { display: display.to_string(), added, removed, will_delete: false, too_large: false }
    }

    #[test]
    fn toggle_switches_between_yes_and_no() {
        let mut rc = RewindConfirm {
            target: flashagent_core::Rewindable { user_index: 0, turn: 0, files: 1 },
            prompt_label: "change the notes".to_string(),
            files: vec![row("notes.txt", 1, 1)],
            confirm: true,
        };
        assert!(rc.confirm);
        rc.toggle();
        assert!(!rc.confirm);
        rc.toggle();
        assert!(rc.confirm);
    }

    #[test]
    fn the_card_shows_the_prompt_the_files_and_both_choices() {
        let rc = RewindConfirm {
            target: flashagent_core::Rewindable { user_index: 0, turn: 0, files: 1 },
            prompt_label: "change the notes".to_string(),
            files: vec![row("notes.txt", 1, 3)],
            confirm: true,
        };
        let lines: Vec<String> = rc.render(80).into_iter().map(|(_, t)| t).collect();
        let joined = lines.join("\n");
        assert!(joined.contains("Confirm Rewind"));
        assert!(joined.contains("change the notes"));
        assert!(joined.contains("notes.txt"));
        assert!(joined.contains("+1"));
        assert!(joined.contains("-3"));
        assert!(joined.contains("Yes, rewind"));
        assert!(joined.contains("No, cancel"));
    }

    #[test]
    fn no_file_changes_says_so_plainly() {
        let rc = RewindConfirm {
            target: flashagent_core::Rewindable { user_index: 0, turn: 0, files: 0 },
            prompt_label: "just a question".to_string(),
            files: vec![],
            confirm: true,
        };
        let joined: String = rc.render(80).into_iter().map(|(_, t)| t).collect::<Vec<_>>().join("\n");
        assert!(joined.contains("will not change any files"));
    }
}
