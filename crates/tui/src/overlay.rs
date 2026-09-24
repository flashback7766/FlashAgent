use super::*;

use flashagent_tui::memory_view::MemoryModal;

/// The panel in the composer's place: a menu, settings or a modal. At most
/// one is open; opening another replaces it.
pub(crate) enum Overlay {
    Effort(SelectMenu<String>),
    Model(SelectMenu<String>),
    /// /provider: switch, add or edit.
    Provider(SelectMenu<String>),
    /// Settings -> Providers.
    Providers(Box<flashagent_tui::ProvidersView>),
    /// /resume
    Sessions(SelectMenu<String>),
    Rewind(RewindConfirm),
    /// Boxed: the settings screen carries a whole config.
    Settings(Box<SettingsView>),
    Sampling(SamplingView),
    Context(ContextModal),
    Memory(MemoryModal),
    Mcp(McpModal),
    /// Ctrl+K: every command, searched by typing.
    Palette(SelectMenu<String>),
}

impl Overlay {
    /// For mouse-wheel scrolling.
    pub(crate) fn select_menu_mut(&mut self) -> Option<&mut SelectMenu<String>> {
        match self {
            Overlay::Effort(menu) | Overlay::Model(menu) | Overlay::Provider(menu) | Overlay::Sessions(menu) | Overlay::Palette(menu) => Some(menu),
            _ => None,
        }
    }

    pub(crate) fn render(&self, width: usize) -> Vec<RenderLine> {
        match self {
            Overlay::Effort(menu) | Overlay::Model(menu) | Overlay::Provider(menu) | Overlay::Sessions(menu) | Overlay::Palette(menu) => {
                menu.render(width)
            }
            Overlay::Providers(view) => view.render(width),
            Overlay::Rewind(card) => card.render(width),
            Overlay::Settings(view) => view.render(width),
            Overlay::Sampling(view) => view.render(width),
            Overlay::Context(modal) => modal.render(width),
            Overlay::Memory(modal) => modal.render(width),
            Overlay::Mcp(modal) => modal.render(width),
        }
    }
}

impl App {
    pub(crate) fn settings_view_mut(&mut self) -> Option<&mut SettingsView> {
        match &mut self.overlay {
            Some(Overlay::Settings(view)) => Some(view),
            _ => None,
        }
    }

    /// Each command with the key that also does it, so "f1" finds /context.
    pub(crate) fn open_palette(&mut self) {
        let key_for = |trigger: &str| match trigger {
            "/context" => "F1",
            "/verbose" => "F2",
            "/model" => "F3",
            "/effort" => "F4",
            "/settings" => "Tab",
            "/regenerate" => "Ctrl+R",
            "/editor" => "Ctrl+E",
            "/update" => "Ctrl+U",
            "/exit" => "Ctrl+D",
            _ => "",
        };
        let mut commands = flashagent_tui::autocomplete::builtin_commands();
        commands.extend(flashagent_tui::autocomplete::load_skills(std::path::Path::new(".")));
        let items = commands
            .into_iter()
            .map(|c| {
                let key = key_for(&c.trigger);
                let label = if key.is_empty() { c.trigger.clone() } else { format!("{}  {key}", c.trigger) };
                let value = if flashagent_tui::autocomplete::needs_argument(&c.trigger) { format!("{} ", c.trigger) } else { c.trigger };
                SelectItem::with_description(label, c.description, value)
            })
            .collect();
        self.open_overlay(Overlay::Palette(SelectMenu::new("Command palette", items).with_noun("commands")));
    }

    pub(crate) fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.renderer.request_reprint();
    }

    /// As the session runs now, not as last saved.
    pub(crate) fn runtime_settings(&self, mode: PermissionMode) -> SettingsView {
        let runtime_mode = self.goal_state.as_ref().map_or(mode, |g| g.mode);
        let effort = self.goal_state.as_ref().map_or(self.current_effort.as_str(), |g| g.effort.as_str());
        settings_for_runtime(
            &self.config,
            runtime_mode,
            effort,
            &self.current_model,
            &self.available_models,
            self.context_usage.total_capacity,
        )
    }
}
