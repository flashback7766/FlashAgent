use super::*;

pub(crate) fn build_model_menu(source: &BackendSource) -> Option<SelectMenu<String>> {
    let disc = source.discovery()?;
    if disc.models.is_empty() {
        return None;
    }
    let url = disc.base_url.to_lowercase();
    let is_lm_studio = url.contains("1234") || url.contains("lmstudio");
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
    Some(SelectMenu::new("Select Model", items).with_noun("models"))
}

pub(crate) fn build_effort_menu(
    source: &BackendSource,
    memory: &flashagent_core::EffortMemory,
    model: &str,
) -> SelectMenu<String> {
    let mut items = Vec::new();
    // Auto is the one setting that changes behind the user's back, so it is
    // the one that has to say what it has decided and why.
    // The profile is empty both for "cannot reason" and for "not discovered
    // yet"; discovery is the one that can tell them apart.
    let can_think = match source.profile() {
        Some(p) => p.supported,
        None => source
            .discovery()
            .and_then(|d| d.models.iter().find(|m| m.id == model).map(|m| m.thinking.supported))
            .unwrap_or(true),
    };
    let auto_desc = if !can_think {
        // The setting is kept, but saying it is in force would be a lie.
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
    SelectMenu::new("Select Thinking Effort", items).with_noun("options")
}

pub(crate) fn thinking_summary_str(source: &BackendSource, current_effort: &str) -> String {
    if let Some(ref p) = source.profile() {
        if p.supported && !p.presets.is_empty() {
            format!("{} [{}]", current_effort, p.presets.join(", "))
        } else if !p.supported {
            "disabled (unsupported)".to_string()
        } else {
            current_effort.to_string()
        }
    } else {
        current_effort.to_string()
    }
}

/// A settings view that shows the live session state (mode, effort, model),
/// not just the startup defaults stored in the config file.
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
    cfg.model = model.to_string();
    let mut view = SettingsView::new(cfg, models.to_vec());
    view.context_capacity = context_capacity;
    view
}

/// The config to persist from a settings view. The view shows the live mode
/// and effort; they become startup defaults only when the user changed them
/// there — merely opening Settings mid-session (or during /goal's Accept All)
/// must not silently make that the default for the next launch.
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
