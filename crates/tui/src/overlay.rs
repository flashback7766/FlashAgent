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
    /// /tasks: background commands.
    Tasks(flashagent_tui::TasksModal),
    /// Ctrl+K: every command, searched by typing.
    Palette(SelectMenu<String>),
    /// /tools: what the model may call, and what it may not.
    Tools(flashagent_tui::tools_view::ToolsView),
    /// /notices: what the app said, kept so it can be read again.
    Notices(flashagent_tui::notice_log::NoticeLog),
    /// /rules: the standing permissions, and a way to take them back.
    Rules(flashagent_tui::rules_view::RulesView),
}

impl Overlay {
    /// For mouse-wheel scrolling.
    pub(crate) fn select_menu_mut(&mut self) -> Option<&mut SelectMenu<String>> {
        match self {
            Overlay::Effort(menu) | Overlay::Model(menu) | Overlay::Provider(menu) | Overlay::Sessions(menu) | Overlay::Palette(menu) => Some(menu),
            _ => None,
        }
    }

    /// The wheel over a screen that is not a menu: every list moves its own
    /// selection, the way its arrows do but stopping at the ends. True when
    /// the screen moved; a static card leaves the wheel unspent rather than
    /// passing it to the conversation behind it.
    pub(crate) fn wheel(&mut self, dir: i32) -> bool {
        match self {
            Overlay::Settings(view) => view.wheel(dir),
            Overlay::Providers(view) => view.wheel(dir),
            Overlay::Sampling(view) => view.wheel(dir),
            Overlay::Memory(modal) => modal.wheel(dir),
            Overlay::Mcp(modal) => modal.wheel(dir),
            Overlay::Tasks(modal) => modal.wheel(dir),
            Overlay::Tools(view) => view.wheel(dir),
            Overlay::Notices(view) => view.wheel(dir),
            Overlay::Rules(view) => view.wheel(dir),
            Overlay::Context(_) | Overlay::Rewind(_) => false,
            Overlay::Effort(_) | Overlay::Model(_) | Overlay::Provider(_) | Overlay::Sessions(_) | Overlay::Palette(_) => false,
        }
    }

    /// `height` is the whole window: a menu keeps to what is left of it once
    /// the footer under it has its rows. `hover` is where the pointer sits on
    /// the card (a line from its first row, a column from the screen's edge)
    /// and `flash` the line a click just landed on: the screens that answer
    /// the mouse mark the row a press would reach.
    pub(crate) fn render(&self, width: usize, height: usize, hover: Option<(usize, u16)>, flash: Option<usize>) -> Vec<RenderLine> {
        match self {
            Overlay::Effort(menu) | Overlay::Model(menu) | Overlay::Provider(menu) | Overlay::Sessions(menu) | Overlay::Palette(menu) => {
                menu.render_in(width, height.saturating_sub(MENU_FOOTER_ROWS))
            }
            Overlay::Providers(view) => view.render(width),
            Overlay::Rewind(card) => card.render(width),
            Overlay::Settings(view) => view.render(width, hover, flash),
            Overlay::Sampling(view) => view.render(width),
            Overlay::Context(modal) => modal.render(width),
            Overlay::Memory(modal) => modal.render(width),
            Overlay::Mcp(modal) => modal.render(width),
            Overlay::Tasks(modal) => modal.render(width),
            Overlay::Tools(view) => view.render(width, hover, flash),
            Overlay::Notices(view) => view.render(width, hover, flash),
            Overlay::Rules(view) => view.render(width, hover, flash),
        }
    }
}

/// The rows a frame keeps under an open menu (the blank row above it, a blank,
/// a tip of up to two lines, the status line): with five, a two-line tip cut
/// off the menu's border.
const MENU_FOOTER_ROWS: usize = 6;

impl App {
    pub(crate) fn settings_view_mut(&mut self) -> Option<&mut SettingsView> {
        match &mut self.overlay {
            Some(Overlay::Settings(view)) => Some(view),
            _ => None,
        }
    }

    /// A press on the open overlay's card: `line` counts from the card's
    /// first row on screen, `column` from the screen's left edge. What a row
    /// does is the view's to say; what the action starts goes the same way
    /// the keys send it. The overlay is taken out and put back as the keys
    /// do, because a click can open another screen.
    pub(crate) async fn overlay_click(&mut self, cx: &mut LoopCtx<'_>, line: usize, column: usize) -> bool {
        let width = self.last_term_size.0 as usize;
        match self.overlay.take() {
            Some(Overlay::Settings(mut view)) => {
                let shown_mode = self.goal_state.as_ref().map_or(cx.perm.state().mode(), |g| g.mode);
                let shown_effort = self.goal_state.as_ref().map_or(self.current_effort.clone(), |g| g.effort.clone());
                let action = view.click(line, column, width);
                self.process_settings_action(cx, view, action, shown_mode, &shown_effort).await;
                true
            }
            Some(Overlay::Tools(mut view)) => {
                let action = view.click(line);
                self.tools_action(cx, view, action);
                true
            }
            // Selecting only: taking a rule or a notice back stays with the
            // keys, because a click is a poor place for a destructive surprise.
            Some(Overlay::Rules(mut view)) => {
                let took = view.click_select(line);
                self.overlay = Some(Overlay::Rules(view));
                took
            }
            Some(Overlay::Notices(mut view)) => {
                let took = view.click_select(line);
                self.overlay = Some(Overlay::Notices(view));
                took
            }
            Some(other) => {
                self.overlay = Some(other);
                false
            }
            None => false,
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
            "/editor" => "Ctrl+X Ctrl+E",
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
        self.open_overlay(Overlay::Palette(SelectMenu::new(flashagent_tui::i18n::tr("Command palette"), items).with_noun(flashagent_tui::select::Noun::COMMANDS)));
    }

    pub(crate) fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
        self.renderer.request_reprint();
    }

    /// As the session runs now, not as last saved.
    pub(crate) fn runtime_settings(&self, mode: PermissionMode, source: &BackendSource) -> SettingsView {
        let runtime_mode = self.goal_state.as_ref().map_or(mode, |g| g.mode);
        let effort = self.goal_state.as_ref().map_or(self.current_effort.as_str(), |g| g.effort.as_str());
        settings_for_runtime(
            &self.config,
            runtime_mode,
            effort,
            &self.current_model,
            &self.available_models,
            self.context_usage.total_capacity,
            crate::menus::effort_choices(source),
        )
    }
}
