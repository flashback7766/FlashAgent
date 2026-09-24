//! Switching provider while the app runs. The client is pointed at the new
//! server and asks it what it runs; the model, its window, its thinking
//! settings and the warm prompt cache follow. The conversation stays: the
//! history is the same whatever protocol carries it.

use super::*;
use flashagent_core::config::normalize_url;
use flashagent_tui::providers::{self, ProviderArg, ProvidersAction};
use flashagent_tui::ProvidersView;

/// A switch whose server has not answered yet.
pub(crate) struct ProviderSwitch {
    pub(crate) name: String,
    /// As the client has it; `ProviderReady` names it back.
    pub(crate) url: String,
}

/// The window assumed for a model no server has described.
const UNKNOWN_CONTEXT: usize = 131_072;

/// Counts switches, so only the latest one reports its server's answer.
static SWITCHES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl App {
    /// The client talks to the provider the config calls active.
    pub(crate) fn on_active_provider(&self, source: &BackendSource) -> bool {
        let active = self.config.active_profile();
        normalize_url(&active.url) == normalize_url(&source.0.base_url()) && active.protocol == source.0.protocol()
    }

    /// On the line under the prompt, which shows while a turn runs and while
    /// something is typed.
    pub(crate) fn refuse_switch_while_busy(&mut self) {
        self.background = Some(BackgroundNotice::fading(providers::BUSY.to_string(), 6).warning());
        self.renderer.request_reprint();
    }

    /// `/provider [name]`.
    pub(crate) fn provider_command(&mut self, cx: &LoopCtx<'_>, arg: &str) {
        match providers::provider_arg(&self.config, arg) {
            ProviderArg::Switch(name) => self.switch_provider(cx, &name),
            ProviderArg::Menu(filter) => {
                let mut menu = providers::provider_menu(&self.config);
                filter.chars().for_each(|c| menu.push_filter_char(c));
                self.open_overlay(Overlay::Provider(menu));
            }
        }
    }

    pub(crate) fn open_providers_view(&mut self) {
        self.open_overlay(Overlay::Providers(Box::new(ProvidersView::new(&self.config))));
    }

    /// To a saved provider, by name; it becomes the one to start with next time.
    pub(crate) fn switch_provider(&mut self, cx: &LoopCtx<'_>, name: &str) {
        if self.running {
            self.refuse_switch_while_busy();
            return;
        }
        let already = self.config.run_provider.is_none()
            && self.config.active_profile().name == name
            && self.provider_switch.is_none()
            && self.on_active_provider(cx.source);
        if already {
            self.notice(format!("Already on {name}"));
            return;
        }
        if !self.config.activate(name) {
            self.notice(format!("No provider called {name}"));
            return;
        }
        self.save_config();
        self.connect_active(cx);
    }

    /// Points the client at the active provider and asks its server what it
    /// runs; `provider_ready` takes the answer.
    pub(crate) fn connect_active(&mut self, cx: &LoopCtx<'_>) {
        let profile = self.config.active_profile().clone();
        let endpoint = profile.endpoint();
        self.provider_switch = Some(ProviderSwitch { name: profile.name.clone(), url: endpoint.url.clone() });
        // A warm-up still reading the old server's prefix is no use to the new one.
        if let Some((_, task)) = self.warm_task.take() {
            task.abort();
        }
        self.cache_warm_key = None;
        // The old server's absence is not the new one's; the new one's is said anew.
        self.announced_mood = MascotMood::Checking;
        if self.background.as_ref().is_some_and(|b| b.text.starts_with(OFFLINE_NOTICE)) {
            self.background = None;
        }
        self.notice(format!("Connecting to {} \u{b7} {}\u{2026}", profile.name, endpoint.url));
        // Here, not in the task, so quick switches reach the client in order. A
        // look at the old server still under way is dropped by the client.
        cx.source.0.set_endpoint(endpoint.clone());
        cx.source.0.set_model(&profile.model);
        let (source, tx, busy) = (cx.source.clone(), cx.tx.clone(), cx.is_discovering.clone());
        let this_switch = SWITCHES.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::spawn(async move {
            busy.store(true, Ordering::SeqCst);
            let discovery = source.discover_server().await;
            busy.store(false, Ordering::SeqCst);
            if SWITCHES.load(Ordering::SeqCst) == this_switch {
                let _ = tx.send(UiEvent::ProviderReady { url: endpoint.url, discovery });
            }
        });
    }

    /// The new server answered, or did not. Its saved model is taken up if it
    /// has it, and saved as the provider's model if another had to be chosen.
    pub(crate) fn provider_ready(&mut self, cx: &LoopCtx<'_>, url: &str, discovery: Option<flashagent_llm::ServerDiscovery>) {
        if self.provider_switch.as_ref().is_none_or(|s| s.url != url) {
            // A later switch overtook this one.
            return;
        }
        let Some(switch) = self.provider_switch.take() else { return };
        let saved = self.config.active_profile().model.clone();
        let model = providers::model_after_switch(&saved, discovery.as_ref());
        self.available_models = discovery.as_ref().map(providers::offered_models).unwrap_or_default();
        // The old model's window says nothing about the new one.
        self.current_context = None;
        self.context_usage.total_capacity = UNKNOWN_CONTEXT;
        cx.tools_arc.set_context_window(Some(UNKNOWN_CONTEXT));
        self.switch_model(cx, &model);
        if !model.is_empty() && model != saved {
            self.config.active_profile_mut().model = model.clone();
            self.save_config();
        }
        self.refresh_welcome(cx.source, cx.mascot_mood);

        let profile = self.config.active_profile();
        let how = format!("{} \u{b7} {}", profile.protocol.label(), profile.url);
        let line = match (&discovery, model.is_empty()) {
            (Some(_), false) => format!("Provider: {} \u{b7} {model} ({how})", switch.name),
            (Some(_), true) => format!("Provider: {} ({how}) lists no models yet", switch.name),
            (None, _) => format!("Provider: {} ({how}) did not answer; its models are listed once it does", switch.name),
        };
        self.chat.push_system(&line);
        match discovery {
            Some(_) if !model.is_empty() => self.notice(format!("Switched to {} \u{b7} {model}", switch.name)),
            _ => self.notice(format!("Switched to {} \u{b7} /provider goes back", switch.name)),
        }
        self.renderer.request_reprint();
    }

    /// `/provider`'s menu: a provider switches, the last two rows add or edit.
    pub(crate) async fn provider_menu_key(&mut self, cx: &mut LoopCtx<'_>, mut menu: SelectMenu<String>, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Enter => {
                let picked = menu.selected_value().cloned().filter(|_| !menu.filtered_indices().is_empty());
                match picked.as_deref() {
                    None => self.overlay = Some(Overlay::Provider(menu)),
                    Some(providers::ADD) => self.run_setup_in_app(cx, true).await,
                    Some(providers::MANAGE) => self.open_providers_view(),
                    Some(providers::THIS_RUN) => self.notice(format!("Already on {} for this run", self.config.active_profile().name)),
                    Some(name) => self.switch_provider(cx, name),
                }
            }
            KeyCode::Esc => {}
            _ => {
                navigate_menu(&mut menu, code, mods);
                self.overlay = Some(Overlay::Provider(menu));
            }
        }
    }

    /// Settings -> Providers.
    pub(crate) async fn providers_key(&mut self, cx: &mut LoopCtx<'_>, mut view: Box<ProvidersView>, code: KeyCode, mods: KeyModifiers) {
        match view.handle_key(code, mods) {
            ProvidersAction::None => {}
            ProvidersAction::Close => return,
            ProvidersAction::Switch(name) => return self.switch_provider(cx, &name),
            ProvidersAction::Add => return self.run_setup_in_app(cx, true).await,
            ProvidersAction::Save { original, profile } => self.save_edited_provider(cx, &mut view, &original, profile),
            ProvidersAction::Delete(name) => {
                if self.config.remove_provider(&name) {
                    self.save_config();
                    view.refresh(&self.config);
                    view.status = Some(format!("Deleted {name}"));
                }
            }
        }
        self.overlay = Some(Overlay::Providers(view));
    }

    /// An edit to the provider in use reconnects to it, which waits for a
    /// running answer to finish.
    fn save_edited_provider(&mut self, cx: &LoopCtx<'_>, view: &mut ProvidersView, original: &str, profile: flashagent_core::config::ProviderProfile) {
        let in_use = self.config.run_provider.is_none() && self.config.active_profile().name == original;
        let before = self.config.profile(original).cloned();
        let reconnect = in_use
            && before.is_some_and(|b| b.url != profile.url || b.protocol != profile.protocol || b.api_key != profile.api_key || b.model != profile.model);
        if reconnect && self.running {
            view.refused(providers::BUSY);
            return;
        }
        let name = profile.name.trim().to_string();
        if let Err(why) = self.config.replace_provider(original, profile) {
            view.refused(why);
            return;
        }
        self.save_config();
        view.saved(&self.config, &name);
        if reconnect {
            self.connect_active(cx);
        } else {
            self.refresh_welcome(cx.source, cx.mascot_mood);
        }
    }

    /// The setup steps in the app, which has the whole screen meanwhile; what
    /// they save is connected to at once. `adding` asks only for a provider.
    pub(crate) async fn run_setup_in_app(&mut self, cx: &mut LoopCtx<'_>, adding: bool) {
        if self.running {
            self.refuse_switch_while_busy();
            return;
        }
        let mut deferred = Vec::new();
        let completed = flashagent_tui::run_wizard_channel(&mut self.config, adding, cx.rx, &mut deferred).await.unwrap_or(false);
        for ev in deferred {
            let _ = cx.tx.send(ev);
        }
        // The wizard had the whole screen.
        self.renderer.request_reprint();
        if completed {
            if !adding {
                self.current_effort = self.config.thinking_effort.clone();
                cx.perm.state().set_mode(self.config.permission_mode);
            }
            self.connect_active(cx);
        }
    }
}
