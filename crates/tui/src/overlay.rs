use super::*;

use flashagent_tui::memory_view::MemoryModal;

/// The panel that has taken the composer's place: a menu, a settings screen,
/// a modal. There is one place for it, so there is at most one of them —
/// opening another replaces it, and "is anything open" is one question.
pub(crate) enum Overlay {
    Effort(SelectMenu<String>),
    Model(SelectMenu<String>),
    /// Saved sessions of this folder (/resume).
    Sessions(SelectMenu<String>),
    Rewind(RewindConfirm),
    /// Boxed: the settings screen carries a whole config.
    Settings(Box<SettingsView>),
    Sampling(SamplingView),
    Context(ContextModal),
    Memory(MemoryModal),
    Mcp(McpModal),
}

impl Overlay {
    /// The menu to scroll with the mouse wheel, for the overlays that are one.
    pub(crate) fn select_menu_mut(&mut self) -> Option<&mut SelectMenu<String>> {
        match self {
            Overlay::Effort(menu) | Overlay::Model(menu) | Overlay::Sessions(menu) => Some(menu),
            _ => None,
        }
    }

    /// Lines to draw in place of the composer.
    pub(crate) fn render(&self, width: usize) -> Vec<RenderLine> {
        match self {
            Overlay::Effort(menu) | Overlay::Model(menu) | Overlay::Sessions(menu) => menu.render(width),
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

    /// Open `overlay`, replacing any other.
    pub(crate) fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.renderer.request_reprint();
    }

    /// Settings as the session is running now, not as last saved.
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
