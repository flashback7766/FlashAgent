//! Terminal client: wires backend, loop, tools and permissions in-process and
//! renders through `flashagent_tui`.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use flashagent_core::{
    build_system_prompt, AgentLoop, AppConfig, ContextUsage, Decision, DoneReason, LlmSource,
    LoopConfig, LoopEvent, PermissionMode, PermissionState, PermissionedTools, SystemPromptConfig,
};
use flashagent_llm::{ChatMessage, LlmBackend};
use flashagent_tools::{BuiltinTools, BuiltinToolsConfig};
use flashagent_tui::goal::{commit_goal_milestone, format_plan, GoalBudgets, GoalLedger, MILESTONE_COMMIT_INTERVAL};
use flashagent_tui::MascotMood;
use flashagent_tui::{
    clip_ansi, pad_box_row, render_session_saved_card, restore_line_color,
    visible_width, welcome_card, AutocompletePopup, ChatView, ConfirmChoice, ConfirmSelect,
    ContextModal, LineKind, McpModal, McpModalAction, McpViewTab, PrefillTracker, ReasoningExpansion,
    RenderLine, SamplingAction, SamplingView, SelectItem, SelectMenu, SettingsAction, SettingsView,
    TuiGate, TuiQuestionGate, UiEvent,
};

mod render;
mod tokens;
mod backend;
mod menus;
mod notices;
mod attachments;
mod sessions;
mod prompt_history;
mod export;
mod recap;
mod compact;
mod cards;
mod toolcheck_cli;
mod submit;
mod overlay_keys;
mod keys;
mod turns;
mod overlay;
mod memory_summary;
mod warm;
mod provider_switch;
mod tasks;
mod events;
use render::*;
use overlay::Overlay;
use tokens::*;
use backend::*;
use menus::*;
use notices::*;
use attachments::*;
use sessions::*;
use recap::*;
use memory_summary::*;
use compact::*;
use cards::*;
use toolcheck_cli::*;
use provider_switch::ProviderSwitch;
use overlay_keys::navigate_menu;

#[derive(Default, Clone)]
struct QuestionUiState {
    selected_index: usize,
    write_in_text: String,
    is_writing: bool,
    selected_indices: std::collections::BTreeSet<usize>,
}

async fn cli_start(config: &mut AppConfig) -> Result<Option<(bool, bool, SessionStart)>> {
    let mut force_setup = false;
    let mut skip_trust = false;
    let mut session_start = SessionStart::New;
    let mut tool_test: Option<bool> = None;
    let (mut cli_url, mut cli_model) = (None, None);
    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => cli_url = args.next(),
            "--model" => cli_model = args.next(),
            "-v" | "--version" => {
                println!("FlashAgent {}", flashagent_svc::updater::current_version());
                return Ok(None);
            }
            "--resume" | "-r" => {
                // Without an id: pick from this folder's sessions.
                session_start = match args.next_if(|next| !next.starts_with('-')) {
                    Some(id) => SessionStart::Resume(id),
                    None => SessionStart::Pick,
                };
            }
            "--continue" | "-c" => session_start = SessionStart::Continue,
            "--uninstall" => {
                // `-y` anywhere takes every default, for scripts.
                let assume_yes = std::env::args().any(|a| a == "-y" || a == "--yes");
                if let Err(e) = flashagent_svc::uninstall::run_interactive(assume_yes) {
                    eprintln!("Uninstall stopped: {e}");
                    std::process::exit(1);
                }
                return Ok(None);
            }
            "--update" => {
                if flashagent_svc::updater::is_dev_mode() {
                    println!("In-app updater is disabled in development mode (running from source repository or cargo target build).");
                    println!("To update your dev build, pull latest git commits and run `cargo build --release`.");
                    return Ok(None);
                }
                println!("Checking for updates on {} channel...", config.update_channel.label());
                match flashagent_svc::updater::check_for_updates(config.update_channel, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                    Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, is_downgrade, checksums_url, .. }) => {
                        let op = if is_downgrade { "Downgrading" } else { "Updating" };
                        println!("{op} FlashAgent to {target} (downloading {asset_name})...");
                        let path = flashagent_svc::updater::download_and_apply(&download_url, &asset_name, checksums_url.as_deref()).await?;
                        println!("Update successfully installed to {}", path.display());
                    }
                    Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, channel }) => {
                        println!("FlashAgent {current} is already up to date on {} channel.", channel.label());
                    }
                    Err(e) => {
                        eprintln!("Update check failed: {e}");
                    }
                }
                return Ok(None);
            }
            "--channel" => {
                if let Some(val) = args.next() {
                    match val.to_lowercase().as_str() {
                        "stable" | "v" => config.update_channel = flashagent_core::config::UpdateChannel::Stable,
                        "beta" | "b" => config.update_channel = flashagent_core::config::UpdateChannel::Beta,
                        other => anyhow::bail!("invalid channel: {other} (use stable or beta)"),
                    }
                    let _ = config.save();
                    println!("Release channel set to {}", config.update_channel.label());
                    return Ok(None);
                } else {
                    println!("Current channel: {}", config.update_channel.label());
                    return Ok(None);
                }
            }
            "--tool-test" => tool_test = Some(false),
            "--all-models" => tool_test = Some(true),
            "--setup" => force_setup = true,
            "-y" | "--yes" => skip_trust = true,
            "-h" | "--help" => {
                println!("FlashAgent TUI\n\nUsage: flashagent [OPTIONS]\n\nOptions:\n  -v, --version        Print version\n  --update             Check and apply updates\n  --channel <name>     Switch release channel (stable, beta)\n  --model <name>       Model to use, saved for the provider in use\n  --url <endpoint>     Talk to this server for this run only (saved providers: /provider)\n  --setup              Run first-time setup wizard\n  --tool-test [--all-models]  Check whether the model can drive tools\n  -r, --resume [id]    Resume a saved session (without an id: pick one from this folder)\n  -c, --continue       Continue the latest session in this folder\n  -y, --yes            Skip directory trust confirmation\n  --uninstall [-y]     Remove FlashAgent; asks what data to delete (-y: take the defaults)\n  -h, --help           Show this help message");
                return Ok(None);
            }
            other => anyhow::bail!("usage: flashagent [-v] [--update] [--channel <stable|beta>] [--model <name>] [--url http://host/v1] [--tool-test [--all-models]] [--setup] [-r|--resume [id]] [-c|--continue] [-y|--yes] (got {other})"),
        }
    }

    // After every flag is read, so their order does not matter: the model goes
    // to the server this run uses.
    if let Some(url) = cli_url {
        config.use_url_for_this_run(&url);
    }
    if let Some(model) = cli_model {
        config.active_profile_mut().model = model;
    }

    if let Some(all_models) = tool_test {
        let code = run_tool_check_cli(config, all_models).await;
        std::process::exit(code);
    }

    Ok(Some((force_setup, skip_trust, session_start)))
}

async fn startup_screens(config: &mut AppConfig, force_setup: bool) -> Option<bool> {
    // The screens before the chat (setup, release notes, trust) wear the chosen
    // look too.
    flashagent_tui::theme::set(config.color_theme);
    flashagent_tui::anim::set_enabled(config.animations);
    // Carried into the first conversation: the screen it was printed on is about
    // to be cleared.
    let mut ran_setup = false;
    if (!config.setup_completed || force_setup) && std::io::stdout().is_terminal() {
        let completed = flashagent_tui::run_wizard(config).await.unwrap_or(false);
        if !completed {
            return None;
        }
        // Nothing new to show: stamp the version so the next update has a baseline.
        config.last_seen_version = Some(flashagent_svc::updater::current_version().to_string());
        let _ = config.save();
        ran_setup = true;
    }

    // Once, after the binary moved forward, show what arrived. Never right after
    // the first setup: there is nothing new to someone who just arrived.
    if std::io::stdout().is_terminal() && !ran_setup {
        let now = flashagent_svc::updater::current_version();
        let news = match config.last_seen_version.as_deref() {
            Some(seen) => flashagent_tui::whatsnew::since(Some(seen), now),
            // Users who updated into the first build that records a version have none
            // recorded; a set-up config without one is an existing user, not a first run.
            None if config.setup_completed => flashagent_tui::whatsnew::latest(1),
            None => Vec::new(),
        };
        if config.last_seen_version.as_deref() != Some(now) {
            config.last_seen_version = Some(now.to_string());
            let _ = config.save();
        }
        if !news.is_empty() {
            let _ = flashagent_tui::whatsnew::run(news, now).await;
        }
    }

    Some(ran_setup)
}

struct BackendStartup {
    source: Arc<BackendSource>,
    model: String,
    context_display: Option<String>,
    context_capacity: usize,
    cwd_display: String,
    cwd: std::path::PathBuf,
    initial_effort: String,
    available_models: Vec<String>,
    first_run_verdict: Option<String>,
    pending_discovery: Option<tokio::task::JoinHandle<Option<flashagent_llm::ServerDiscovery>>>,
}

fn display_cwd(cwd: &std::path::Path) -> String {
    let home_str = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    if !home_str.is_empty() {
        cwd.strip_prefix(&home_str).map(|rel| {
            let s = rel.to_string_lossy();
            if s.is_empty() { "~".to_string() } else { format!("~/{}", s.trim_start_matches('/')) }
        }).unwrap_or_else(|_| cwd.display().to_string())
    } else {
        cwd.display().to_string()
    }
}

async fn prepare_backend(config: &mut AppConfig, mut skip_trust: bool, ran_setup: bool) -> Result<Option<BackendStartup>> {
    let mut first_run_verdict: Option<String> = None;
    let endpoint = config.endpoint();
    let url = endpoint.url.clone();
    let mut model = config.active_profile().model.clone();

    if std::env::var("FLASHAGENT_TRUST_DIR").is_ok() {
        skip_trust = true;
    }

    // Discovery starts now, so models, context window and presets are known by
    // the time the startup screen is done.
    let backend_initial = flashagent_llm::Client::new(endpoint.clone(), &model);
    let mut discovery_task = tokio::spawn(async move {
        backend_initial.discover_server().await
    });

    let mut cwd = std::env::current_dir()?;

    if config.is_directory_trusted(&cwd) {
        skip_trust = true;
    }

    if !skip_trust && std::io::stdout().is_terminal() {
        let action = flashagent_tui::startup::run_trust_screen(&mut cwd).await?;
        if action == flashagent_tui::StartupAction::Quit {
            discovery_task.abort();
            return Ok(None);
        }
        config.trust_directory(&cwd);
        let _ = config.save();
    }

    if ran_setup {
        // Whether the model can drive tools decides whether anything works. Asked
        // after the trust question, which is instant, so nobody waits for a check
        // before being asked where they are.
        first_run_verdict = first_run_tool_check(config).await;
    }

    // Leaked once so the spawned loop can hold &'static references; the process
    // is the session.
    let keyed = endpoint.api_key.is_some();
    let backend = flashagent_llm::Client::new(endpoint, &model);
    backend.set_max_retries(config.network_retries);
    flashagent_tui::autocomplete::set_provider_names(flashagent_tui::providers::completion_entries(config));
    backend.set_user_sampling(config.sampling_preset == flashagent_core::config::SamplingPreset::Custom);

    let startup_timeout = if model.is_empty() {
        std::time::Duration::from_millis(2000)
    } else {
        std::time::Duration::from_millis(500)
    };
    let (discovery, pending_discovery) = match tokio::time::timeout(startup_timeout, &mut discovery_task).await {
        Ok(res) => (res.unwrap_or(None), None),
        Err(_) => (None, Some(discovery_task)),
    };

    // No model given: the loaded one, else the first; a partial name is matched to its full id.
    model = flashagent_tui::providers::model_after_switch(&model, discovery.as_ref(), keyed);
    backend.set_model(&model);
    // Startup discovered through another backend; this one sends the turns and
    // must know the result from its first request.
    if let Some(ref disc) = discovery {
        backend.adopt_discovery(disc);
    }
    if !model.is_empty() {
        config.active_profile_mut().model = model.clone();
    }

    // With another provider saved, the app opens anyway: /provider reaches it.
    let elsewhere = config.providers.iter().any(|p| !p.same_server(config.active_profile()));
    if model.is_empty() && !elsewhere {
        anyhow::bail!(
            "No model specified and could not connect to LLM server at {url} to auto-detect a loaded model.\n\
             Please start your server (e.g. LM Studio on port 1234) or run with --model <name> or --setup."
        );
    }

    let active_model_info = discovery.as_ref().and_then(|d| d.model(&model).cloned());

    let context_display = active_model_info.as_ref().and_then(|m| m.context_display());
    let context_capacity = active_model_info
        .as_ref()
        .and_then(|m| m.context_length.or(m.max_context_length))
        .unwrap_or(131_072);

    let available_models: Vec<String> = discovery
        .as_ref()
        .map(|d| d.models.iter().map(|m| m.id.clone()).collect())
        .unwrap_or_default();

    let profile = backend.profile().or_else(|| active_model_info.as_ref().map(|m| m.thinking.clone()));

    let initial_effort = if !config.thinking_effort.is_empty() && config.thinking_effort != "default" {
        config.thinking_effort.clone()
    } else if let Some(ref p) = profile {
        // Only the server saying the model cannot reason means off; silence leaves
        // the model's default.
        if !p.supported && !p.is_unreported() {
            "off".to_string()
        } else {
            "auto".to_string()
        }
    } else {
        "auto".to_string()
    };
    let source: Arc<BackendSource> = Arc::new(BackendSource(backend));
    let cwd_display = display_cwd(&cwd);

    Ok(Some(BackendStartup { source, model, context_display, context_capacity, cwd_display, cwd, initial_effort, available_models, first_run_verdict, pending_discovery }))
}

fn install_crash_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        flashagent_tui::screen::set_progress(flashagent_tui::screen::Progress::None);
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::cursor::Show,
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
        );
        let _ = crossterm::terminal::disable_raw_mode();
        let log_content = format!(
            "FlashAgent Crash Report\nVersion: {}\nTime: {:?}\nPanic: {}\nBacktrace:\n{:?}\n\nPlease report this issue at: https://github.com/flashback7766/FlashAgent/issues\n",
            flashagent_svc::updater::current_version(),
            std::time::SystemTime::now(),
            info,
            std::backtrace::Backtrace::capture()
        );
        // Never into the user's project directory.
        let log_path = flashagent_home_dir()
            .map(|d| {
                let _ = std::fs::create_dir_all(&d);
                d.join("crash.log")
            })
            .unwrap_or_else(|| std::env::temp_dir().join("flashagent-crash.log"));
        let _ = std::fs::write(&log_path, log_content);
        eprintln!("\x1b[1;38;2;245;120;120mFlashAgent encountered an unexpected crash.\x1b[0m");
        eprintln!("Crash report written to {}. Please submit an issue at: https://github.com/flashback7766/FlashAgent/issues", log_path.display());
        default_hook(info);
    }));

}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    flashagent_svc::updater::remove_stale_backups_beside_exe();
    let mut config = AppConfig::load();
    let Some((force_setup, skip_trust, session_start)) = cli_start(&mut config).await? else { return Ok(()) };

    let Some(ran_setup) = startup_screens(&mut config, force_setup).await else { return Ok(()) };
    let Some(BackendStartup { source, model, context_display, context_capacity, cwd_display, cwd, initial_effort, available_models, first_run_verdict, pending_discovery }) =
        prepare_backend(&mut config, skip_trust, ran_setup).await? else { return Ok(()) };

    let gate = TuiGate::new();
    let question_gate = TuiQuestionGate::new();
    let mcp_manager = flashagent_tools::mcp::McpManager::new(cwd.clone());
    let mcp_bg = mcp_manager.clone();
    tokio::spawn(async move {
        mcp_bg.start_enabled_servers().await;
    });

    let state = Arc::new(PermissionState::new(config.permission_mode, gate.clone()));
    // Files outside this folder are never read or written on a mode's say alone.
    state.set_project_root(cwd.clone());
    // Only the MCP config (`read_only`, `read_only_tools`) marks external tools as reads.
    let hint_mgr = mcp_manager.clone();
    state.set_read_only_hint(Arc::new(move |tool: &str| hint_mgr.is_tool_read_only(tool)));
    let tools_arc: Arc<BuiltinTools> = Arc::new(BuiltinTools::new(BuiltinToolsConfig {
        cwd: cwd.clone(),
        // DuckDuckGo unless the environment names a Brave key.
        brave_api_key: std::env::var("BRAVE_API_KEY").ok().filter(|k| !k.trim().is_empty()),
        question_gate: Some(question_gate.clone()),
        is_goal_mode: None,
        toolset_profile: Some(config.toolset_profile),
        web_enabled: Some(config.web_tools),
        context_window: Some(context_capacity),
        mcp_manager: Some(mcp_manager.clone()),
    })?);
    // A closed terminal window (SIGHUP) or a kill (SIGTERM) skips the normal
    // exit; the background tasks run in their own process groups and would
    // outlive FlashAgent, holding their ports.
    #[cfg(unix)]
    {
        let tools = tools_arc.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            let (Ok(mut hup), Ok(mut term)) = (signal(SignalKind::hangup()), signal(SignalKind::terminate())) else { return };
            let code = tokio::select! {
                _ = hup.recv() => 129,
                _ = term.recv() => 143,
            };
            tools.shells().stop_all();
            // After SIGTERM the terminal is still there, in raw mode on the alternate screen.
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show, DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen);
            let _ = crossterm::terminal::disable_raw_mode();
            std::process::exit(code);
        });
    }
    // The built-in toolset plus `spawn_agent`.
    let composite = flashagent_tools::agent_tools(tools_arc.clone(), source.clone(), state.clone());
    let perm: &'static PermissionedTools =
        Box::leak(Box::new(PermissionedTools::new(Arc::new(composite), Some(tools_arc.clone()), state.clone())));

    // Project and global memory docs, injected into the first user message.
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
    let docs = flashagent_core::collect(&cwd, &home.join(".flashagent"));
    let memory_block = flashagent_core::injection_block(&docs, config.token_budget);
    let memory_docs = docs.len();

    install_crash_hook();

    run_terminal(AppContext {
        config,
        source,
        perm,
        gate,
        question_gate,
        tools_arc,
        memory_block,
        memory_docs,
        model,
        context_display,
        context_capacity,
        cwd_display,
        initial_effort,
        available_models,
        session_start,
        cwd: cwd.clone(),
        first_run_verdict,
        pending_discovery,
    }).await
}

async fn run_terminal(ctx: AppContext) -> Result<()> {
    enable_raw_mode()?;
    let _ = crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
    );
    let result = run_app(ctx).await;

    flashagent_tui::screen::set_progress(flashagent_tui::screen::Progress::None);
    // The frame may have hidden the cursor (a menu was open); the shell needs it back.
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Show,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    disable_raw_mode()?;
    match result {
        Ok(Some(Ok(ref saved_id))) => {
            let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
            let card = render_session_saved_card(saved_id, term_w as usize);
            println!();
            for line in card {
                println!("{line}");
            }
            println!();
        }
        // The terminal is restored, so this stays in the scrollback.
        Ok(Some(Err(ref why))) => eprintln!("\nThe session was NOT saved: {why}\n"),
        _ => {}
    }
    if result.is_ok() && UNINSTALL_AFTER_EXIT.load(Ordering::SeqCst) {
        if let Err(e) = flashagent_svc::uninstall::run_interactive(false) {
            eprintln!("Uninstall stopped: {e}");
            std::process::exit(1);
        }
    }
    result.map(|_| ())
}

/// Set by "yes" on the /uninstall card: the uninstaller runs once the terminal
/// is restored.
pub(crate) static UNINSTALL_AFTER_EXIT: AtomicBool = AtomicBool::new(false);

/// Also how the hint line recognises the card.
pub(crate) const UNINSTALL_TITLE: &str = "Uninstall FlashAgent";

enum SessionStart {
    New,
    /// `--resume <id>`
    Resume(String),
    /// `--continue`: the newest session of this folder.
    Continue,
    /// `--resume` without an id: pick from this folder's sessions.
    Pick,
}

#[derive(Clone)]
struct SavedGoalState {
    mode: PermissionMode,
    effort: String,
    max_steps: Option<u32>,
    task: String,
}

enum Flow {
    /// Carry on with the rest of this iteration.
    Next,
    /// Start the next iteration.
    Continue,
    Quit,
}

/// The backend, tools and loop channels handlers need besides the app state.
struct LoopCtx<'a> {
    source: &'a Arc<BackendSource>,
    perm: &'static PermissionedTools,
    gate: &'a Arc<TuiGate>,
    question_gate: &'a Arc<TuiQuestionGate>,
    tools_arc: &'a Arc<BuiltinTools>,
    memory_block: &'a String,
    cwd_display: &'a String,
    /// Where the file tools write.
    cwd: &'a std::path::Path,
    cancel: &'a Arc<AtomicBool>,
    tx: &'a tokio::sync::mpsc::UnboundedSender<UiEvent>,
    rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<UiEvent>,
    update_tx: &'a tokio::sync::mpsc::UnboundedSender<UpdateNotice>,
    channel_watch_tx: &'a tokio::sync::watch::Sender<flashagent_core::config::UpdateChannel>,
    channel_probe_tx: &'a tokio::sync::mpsc::UnboundedSender<ChannelTarget>,
    /// While an update is being checked or installed.
    update_busy: &'a Arc<AtomicBool>,
    /// While the client asks a server what it runs; a switch of provider waits
    /// for it.
    is_discovering: &'a Arc<AtomicBool>,
    session_id: &'a String,
    mascot_mood: MascotMood,
    /// For the welcome card's reveal.
    started_at: std::time::Instant,
    /// Already laid out.
    tip_lines: &'a Vec<String>,
}

struct App {
    question_ui_state: QuestionUiState,
    history: Vec<ChatMessage>,
    input: flashagent_tui::Composer,
    custom_placeholder: Option<String>,
    suggested_prompt: Option<String>,
    latest_suggestion: Option<String>,
    tip_animator: flashagent_tui::tips::TipAnimator,
    input_history: Vec<String>,
    history_index: Option<usize>,
    history_search: Option<flashagent_tui::HistorySearch>,
    current_draft: String,
    confirm_select: ConfirmSelect,
    chat: ChatView,
    running: bool,
    active_turn_handle: Option<tokio::task::JoinHandle<()>>,
    active_steer_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    pending_steers: Vec<String>,
    /// Notices of background tasks that ended, on their way to the model.
    task_inbox: flashagent_core::NoticeInbox,
    /// Transcript lines of tasks that ended while a turn was writing.
    task_lines: Vec<String>,
    tasks_refreshed: std::time::Instant,
    /// A quit refused because background tasks run; another soon after quits.
    quit_armed: Option<std::time::Instant>,
    /// The loop is asked to stop cooperatively so it hands back a consistent
    /// history; a hard abort is the fallback.
    cancel_requested: Option<std::time::Instant>,
    aborted_turn: Option<u64>,
    turn_counter: u64,
    all_expanded: bool,
    last_expanded: bool,
    current_model: String,
    current_context: Option<String>,
    current_effort: String,
    /// Kept so the prompt can be rebuilt when the voice changes.
    prompt_config: SystemPromptConfig,
    /// E.g. `~/project`.
    cwd_display: String,
    /// For the welcome card.
    memory_docs: usize,
    /// Colour and time, on the animation clock.
    turn_flash: Option<(flashagent_tui::anim::Rgb, u64)>,
    overlay: Option<overlay::Overlay>,
    last_tool_name: Option<String>,
    attachments: Vec<Attachment>,
    /// Measured once per model.
    image_costs: flashagent_tui::image_cost::ImageCosts,
    image_cost_probe: Option<String>,
    context_usage: ContextUsage,
    autocomplete_idx: usize,
    tick_n: usize,
    renderer: Renderer,
    /// Unprompted things (an update, compaction) go on the line under the input.
    background: Option<BackgroundNotice>,
    channel_switch: Option<ChannelSwitch>,
    uninstall_confirm: bool,
    /// So a new turn can stop it.
    recap_task: Option<tokio::task::JoinHandle<()>>,
    /// When the recap of the last turn is to be written, if the user stays quiet.
    recap_due: Option<std::time::Instant>,
    /// The cached prefix as far as the app knows (see `warm.rs`), and the warm-up
    /// request sending one.
    cache_warm_key: Option<u64>,
    warm_task: Option<(u64, tokio::task::JoinHandle<bool>)>,
    /// Switched at the top of the next loop turn, where the session id and
    /// snapshot store live.
    pending_resume: Option<String>,
    /// Background or manual.
    update_progress: Option<(String, flashagent_svc::updater::UpdateProgress)>,
    /// Ctrl+U or /update; until then a background update goes unannounced.
    update_watched: bool,
    turn_phase: TurnPhase,
    /// Per-model correction of auto effort, learned from how turns went.
    effort_memory: flashagent_core::EffortMemory,
    turn_outcome: flashagent_core::TurnOutcome,
    /// So the next turn is sized before it starts rather than after it overflows.
    last_turn_growth: usize,
    context_before_turn: usize,
    pending_update: Option<(String, String, String, Option<String>)>,
    last_term_size: (u16, u16),
    turn_started: Option<std::time::Instant>,
    token_tracker: TokenTracker,
    max_steps: Option<u32>,
    goal_state: Option<SavedGoalState>,
    /// From loop events, for the progress line and the final report.
    goal_ledger: Option<GoalLedger>,
    copy_toast: Option<(String, std::time::Instant)>,
    last_ctrl_c: Option<std::time::Instant>,
    last_mascot_mood: MascotMood,
    /// So a flickering mood does not re-announce itself.
    announced_mood: MascotMood,
    config: AppConfig,
    available_models: Vec<String>,
    /// A switch of provider whose server has not answered yet.
    provider_switch: Option<ProviderSwitch>,
}

/// How the line under the prompt says the server is not there, so a switch
/// of provider can take it away.
pub(crate) const OFFLINE_NOTICE: &str = "No model server at";

struct AppContext {
    config: AppConfig,
    source: Arc<BackendSource>,
    perm: &'static PermissionedTools,
    gate: Arc<TuiGate>,
    question_gate: Arc<TuiQuestionGate>,
    tools_arc: Arc<BuiltinTools>,
    memory_block: String,
    memory_docs: usize,
    model: String,
    context_display: Option<String>,
    context_capacity: usize,
    cwd_display: String,
    /// Where the file tools write.
    cwd: std::path::PathBuf,
    initial_effort: String,
    available_models: Vec<String>,
    session_start: SessionStart,
    /// To be said in the conversation.
    first_run_verdict: Option<String>,
    pending_discovery: Option<tokio::task::JoinHandle<Option<flashagent_llm::ServerDiscovery>>>,
}

/// Set while an external editor has the terminal; the key reader waits.
static INPUT_PAUSED: AtomicBool = AtomicBool::new(false);

/// The editor setting, or `$VISUAL`, `$EDITOR` and the platform's own. The
/// default setting is the text `$EDITOR`, which is not a program: run as one,
/// Ctrl+E failed on every fresh config.
fn editor_command(preferred_editor: &str) -> String {
    let configured = preferred_editor.trim();
    if !configured.is_empty() && !matches!(configured, "$EDITOR" | "$VISUAL" | "${EDITOR}" | "${VISUAL}") {
        return configured.to_string();
    }
    std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .ok()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| if cfg!(windows) { "notepad" } else { "vi" }.to_string())
}

fn open_in_external_editor(initial_text: &str, preferred_editor: &str) -> std::io::Result<String> {
    let editor = editor_command(preferred_editor);

    let temp_dir = std::env::temp_dir();
    let temp_file = temp_dir.join(format!("flashagent_prompt_{}.md", std::process::id()));
    std::fs::write(&temp_file, initial_text)?;

    INPUT_PAUSED.store(true, Ordering::SeqCst);
    // Let a poll already under way in the reader thread run out.
    std::thread::sleep(std::time::Duration::from_millis(60));
    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Show,
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );

    // `$EDITOR` may carry its own flags ("code --wait").
    let mut words = editor.split_whitespace();
    let status = match words.next() {
        Some(program) => std::process::Command::new(program).args(words).arg(&temp_file).status(),
        None => Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no editor configured")),
    };

    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    );

    INPUT_PAUSED.store(false, Ordering::SeqCst);

    let result = match status {
        // The composer takes several lines: only the trailing newline goes.
        Ok(s) if s.success() => std::fs::read_to_string(&temp_file)
            .map(|text| text.trim_end().replace("\r\n", "\n"))
            .unwrap_or_else(|_| initial_text.to_string()),
        Ok(_) => initial_text.to_string(),
        Err(e) => {
            let _ = std::fs::remove_file(&temp_file);
            return Err(e);
        }
    };

    let _ = std::fs::remove_file(&temp_file);
    Ok(result)
}

/// With a Russian layout Ctrl+D arrives as Ctrl+в. A shortcut is read by the key
/// pressed, so the letter goes back to the Latin one on the same key (ЙЦУКЕН →
/// QWERTY). Plain typing is left alone.
fn latin_shortcut(code: KeyCode, mods: KeyModifiers) -> KeyCode {
    const RU: &str = "йцукенгшщзхъфывапролджэячсмитьбюё";
    const EN: &str = "qwertyuiop[]asdfghjkl;'zxcvbnm,.`";
    let KeyCode::Char(c) = code else { return code };
    if !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return code;
    }
    let lower = c.to_lowercase().next().unwrap_or(c);
    match RU.chars().position(|r| r == lower).and_then(|i| EN.chars().nth(i)) {
        Some(l) if c.is_uppercase() => KeyCode::Char(l.to_ascii_uppercase()),
        Some(l) => KeyCode::Char(l),
        None => code,
    }
}

thread_local! {
    /// Characters whose key is down, so a release can be told from an Alt code.
    static HELD: std::cell::RefCell<Vec<char>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// A character missing from the keyboard layout (`{` on a Russian one) is typed,
/// and pasted through a Windows console, as an Alt code, which arrives only as
/// the release of Alt carrying the character: a release with no press before it.
fn alt_code_char(k: &crossterm::event::KeyEvent) -> Option<char> {
    let KeyCode::Char(c) = k.code else { return None };
    if k.modifiers.contains(KeyModifiers::CONTROL) || c.is_control() {
        return None;
    }
    let was_held = HELD.with(|held| {
        let mut held = held.borrow_mut();
        held.iter().position(|h| *h == c).map(|i| held.swap_remove(i)).is_some()
    });
    (!was_held).then_some(c)
}

/// A character, or a newline for Enter.
fn typed_char(k: &crossterm::event::KeyEvent) -> Option<char> {
    if let KeyCode::Char(c) = k.code {
        HELD.with(|held| {
            let mut held = held.borrow_mut();
            // Bounded: a release the console never sends must not grow it forever.
            if held.len() >= 16 {
                held.remove(0);
            }
            held.push(c);
        });
    }
    let plain = !k.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    // AltGr is Ctrl+Alt on Windows. A layout without `{` on a key of its own (the
    // Russian one) delivers a pasted brace that way, and it is text, not a shortcut.
    let alt_gr = k.modifiers.contains(KeyModifiers::CONTROL | KeyModifiers::ALT);
    match k.code {
        KeyCode::Char(c) if plain => Some(c),
        KeyCode::Char(c) if alt_gr && !c.is_ascii_alphanumeric() && !c.is_control() => Some(c),
        KeyCode::Enter if k.modifiers.is_empty() => Some('\n'),
        _ => None,
    }
}

/// With a newline and more than a line of text among them, they are a paste.
fn gather_burst(first: char) -> Vec<UiEvent> {
    let mut text = String::from(first);
    let mut rest: Vec<UiEvent> = Vec::new();
    // A Windows console hands over a paste a few keys at a time. Nobody presses a
    // key within 30 ms of Enter, so that wait tells a paste from a send.
    let wait = |text: &str| std::time::Duration::from_millis(if text.ends_with('\n') { 30 } else { 5 });
    while matches!(crossterm::event::poll(wait(&text)), Ok(true)) {
        match crossterm::event::read() {
            Ok(Event::Key(k)) if k.kind == KeyEventKind::Release => {
                if let Some(c) = alt_code_char(&k) {
                    text.push(c);
                }
            }
            // Mid-burst a Tab is pasted text, not the Settings key.
            Ok(Event::Key(k)) => match typed_char(&k).or_else(|| is_plain_tab(&k).then_some('\t')) {
                Some(c) => text.push(c),
                None => {
                    rest.push(UiEvent::Key(latin_shortcut(k.code, k.modifiers), k.modifiers));
                    break;
                }
            },
            Ok(Event::Paste(s)) => {
                rest.push(UiEvent::Paste(s));
                break;
            }
            _ => break,
        }
    }
    let mut out = burst_events(&text);
    out.extend(rest);
    out
}

fn is_plain_tab(k: &crossterm::event::KeyEvent) -> bool {
    k.code == KeyCode::Tab && k.modifiers.is_empty()
}

/// A newline only at the end is a word typed and Enter pressed together. A
/// tab with text after it was pasted (`\tfoo()`); a tab at the end is the
/// Tab key, alone (Windows queues its release right behind it) or after a
/// command being completed (`/mo` Tab).
fn burst_events(text: &str) -> Vec<UiEvent> {
    let body = text.trim_end_matches('\n');
    if body.contains('\n') || body.trim_end_matches('\t').contains('\t') {
        let mut out = vec![UiEvent::Paste(body.to_string())];
        // The trailing Enters were keys; one may be the user sending.
        out.extend((body.len()..text.len()).map(|_| UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE)));
        return out;
    }
    text.chars()
        .map(|c| match c {
            '\n' => UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE),
            '\t' => UiEvent::Key(KeyCode::Tab, KeyModifiers::NONE),
            c => UiEvent::Key(KeyCode::Char(c), KeyModifiers::NONE),
        })
        .collect()
}

/// Beside the sessions, under the session id, so it works after --resume.
fn open_snapshots(perm: &PermissionedTools, session_id: &str, cwd: &std::path::Path) {
    let dir = flashagent_home_dir()
        .map(|home| home.join("snapshots").join(session_id))
        .unwrap_or_else(|| std::env::temp_dir().join("flashagent-snapshots").join(session_id));
    perm.state().set_snapshots(Arc::new(flashagent_core::SnapshotStore::open(dir, cwd.to_path_buf())));
}

/// `None`: nothing worth saving; `Ok(id)`: saved; `Err(why)`: not saved.
type SaveOutcome = Option<std::result::Result<String, String>>;

fn start_event_sources(tx: &tokio::sync::mpsc::UnboundedSender<UiEvent>, tools_arc: &Arc<BuiltinTools>, pending_discovery: Option<tokio::task::JoinHandle<Option<flashagent_llm::ServerDiscovery>>>) {
    {
        let mut ended = tools_arc.shells().subscribe();
        let tx = tx.clone();
        tokio::spawn(async move {
            while let Some(notice) = ended.recv().await {
                let _ = tx.send(UiEvent::TaskEnded(notice));
            }
        });
    }

    if let Some(task) = pending_discovery {
        let tx_disc = tx.clone();
        tokio::spawn(async move {
            if let Ok(Some(disc)) = task.await {
                let _ = tx_disc.send(UiEvent::ServerDiscovered(disc));
            }
        });
    }

    // Key and mouse reader thread: crossterm blocks, tokio is async.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                // An external editor owns the terminal; reading here would steal its keys.
                if INPUT_PAUSED.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                match crossterm::event::poll(std::time::Duration::from_millis(50)) {
                    Ok(true) if !INPUT_PAUSED.load(Ordering::SeqCst) => {}
                    Ok(_) => continue,
                    Err(_) => return,
                }
                match crossterm::event::read() {
                    // Mid-paste only: alone, an unmatched release is more likely a key let go
                    // after Shift than a character.
                    Ok(Event::Key(k)) if k.kind == KeyEventKind::Release => {
                        let pasting = matches!(crossterm::event::poll(std::time::Duration::ZERO), Ok(true));
                        if let Some(c) = alt_code_char(&k).filter(|_| pasting) {
                            for ev in gather_burst(c) {
                                if tx.send(ev).is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    Ok(Event::Key(k)) => {
                        // Without bracketed paste (Windows' console) a paste arrives as keys, each
                        // newline an Enter. Keys already waiting with a newline among them are a paste.
                        // A Tab with more keys already waiting starts a paste of
                        // tab-indented code; alone it opens Settings.
                        let pasted_tab = || is_plain_tab(&k) && matches!(crossterm::event::poll(std::time::Duration::ZERO), Ok(true));
                        let events = match typed_char(&k).or_else(|| pasted_tab().then_some('\t')) {
                            Some(first) => gather_burst(first),
                            None => vec![UiEvent::Key(latin_shortcut(k.code, k.modifiers), k.modifiers)],
                        };
                        for ev in events {
                            if tx.send(ev).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(Event::Paste(s)) => {
                        if tx.send(UiEvent::Paste(s)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Mouse(m)) => {
                        if tx.send(UiEvent::Mouse(m)).is_err() {
                            return;
                        }
                    }
                    Ok(Event::Resize(w, h)) => {
                        if tx.send(UiEvent::Resize(w, h)).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });
    }

}

fn resolve_session_start(session_start: SessionStart, cwd_display: &str) -> (Option<String>, bool, Option<String>) {
    // --continue opens the newest session; --resume without an id opens the list.
    let mut open_session_picker = false;
    let mut session_note: Option<String> = None;
    let resume_session_id = match session_start {
        SessionStart::New => None,
        SessionStart::Resume(id) if is_session_id(&id) => Some(id),
        SessionStart::Resume(id) => {
            session_note = Some(format!("'{id}' is not a session id; starting a new session."));
            None
        }
        SessionStart::Pick => {
            open_session_picker = true;
            None
        }
        SessionStart::Continue => {
            let newest = sessions_dir().and_then(|dir| sessions_in(&dir, cwd_display).into_iter().next());
            if newest.is_none() {
                session_note = Some("No saved session in this folder yet; starting a new one.".to_string());
            }
            newest.map(|s| s.id)
        }
    };
    (resume_session_id, open_session_picker, session_note)
}

fn start_background_updates(auto_check_updates: bool, mut channel_watch_rx: tokio::sync::watch::Receiver<flashagent_core::config::UpdateChannel>, update_tx: tokio::sync::mpsc::UnboundedSender<UpdateNotice>, update_busy: Arc<AtomicBool>) {
    if auto_check_updates && !flashagent_svc::updater::is_dev_mode() {
        let update_tx_clone = update_tx.clone();
        let busy = update_busy.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(flashagent_svc::updater::BACKGROUND_UPDATE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let ch = tokio::select! {
                    _ = interval.tick() => *channel_watch_rx.borrow(),
                    changed = channel_watch_rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        interval.reset();
                        *channel_watch_rx.borrow()
                    }
                };
                // A user-started update is under way; look again later.
                if busy.swap(true, Ordering::SeqCst) {
                    continue;
                }
                let progress_tx = update_tx_clone.clone();
                let result = flashagent_svc::updater::check_and_apply_background_with_progress(ch, move |version, stage| {
                    let _ = progress_tx.send(UpdateNotice::Progress { version: version.to_string(), stage });
                })
                .await;
                busy.store(false, Ordering::SeqCst);
                // Up to date and failed are only said to someone watching.
                match result {
                    Ok(Some(version)) => {
                        let _ = update_tx_clone.send(UpdateNotice::Ready { version });
                        break;
                    }
                    Ok(None) => {
                        let _ = update_tx_clone.send(UpdateNotice::UpToDate {
                            version: flashagent_svc::updater::current_version().to_string(),
                        });
                    }
                    Err(e) => {
                        let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                    }
                }
            }
        });
    }

}

struct InitialApp {
    app_config: AppConfig,
    model: String,
    context_display: Option<String>,
    context_capacity: usize,
    cwd_display: String,
    initial_effort: String,
    available_models: Vec<String>,
    system_prompt_text: String,
    system_prompt_config: SystemPromptConfig,
    memory_docs: usize,
    effort_memory: flashagent_core::EffortMemory,
}

fn initial_app(init: InitialApp) -> App {
    let InitialApp { app_config, model, context_display, context_capacity, cwd_display, initial_effort, available_models, system_prompt_text, system_prompt_config, memory_docs, effort_memory } = init;
    App {
        question_ui_state: QuestionUiState::default(),
        history: vec![ChatMessage::system(system_prompt_text)],
        input: flashagent_tui::Composer::new(),
        custom_placeholder: None,
        suggested_prompt: None,
        latest_suggestion: None,
        tip_animator: flashagent_tui::tips::TipAnimator::new(),
        input_history: prompt_history::load(),
        history_index: None,
        history_search: None,
        current_draft: String::new(),
        confirm_select: ConfirmSelect::new(),
        chat: ChatView::default(),
        running: false,
        active_turn_handle: None,
        active_steer_tx: None,
        pending_steers: Vec::new(),
        task_inbox: flashagent_core::NoticeInbox::default(),
        task_lines: Vec::new(),
        tasks_refreshed: std::time::Instant::now(),
        quit_armed: None,
        cancel_requested: None,
        aborted_turn: None,
        turn_counter: 0,
        all_expanded: false,
        last_expanded: false,
        token_tracker: TokenTracker::new(model.clone()),
        current_model: model,
        current_context: context_display,
        current_effort: initial_effort,
        prompt_config: system_prompt_config,
        cwd_display: cwd_display.clone(),
        memory_docs,
        turn_flash: None,
        overlay: None,
        last_tool_name: None,
        attachments: Vec::new(),
        image_costs: flashagent_tui::image_cost::ImageCosts::load(),
        image_cost_probe: None,
        context_usage: ContextUsage::new(context_capacity),
        autocomplete_idx: 0,
        tick_n: 0,
        renderer: Renderer::new(),
        background: None,
        channel_switch: None,
        uninstall_confirm: false,
        recap_task: None,
        recap_due: None,
        cache_warm_key: None,
        warm_task: None,
        pending_resume: None,
        update_progress: None,
        update_watched: false,
        turn_phase: TurnPhase::Waiting,
        effort_memory,
        turn_outcome: flashagent_core::TurnOutcome::default(),
        last_turn_growth: 0,
        context_before_turn: 0,
        pending_update: None,
        last_term_size: crossterm::terminal::size().unwrap_or((100, 24)),
        turn_started: None,
        max_steps: app_config.max_steps,
        goal_state: None,
        goal_ledger: None,
        copy_toast: None,
        last_ctrl_c: None,
        last_mascot_mood: MascotMood::Checking,
        announced_mood: MascotMood::Checking,
        config: app_config,
        available_models,
        provider_switch: None,
    }
}

/// What the handlers share for the whole run; [`LoopCtx`] borrows it per frame.
struct Wiring {
    source: Arc<BackendSource>,
    perm: &'static PermissionedTools,
    gate: Arc<TuiGate>,
    question_gate: Arc<TuiQuestionGate>,
    tools_arc: Arc<BuiltinTools>,
    memory_block: String,
    cwd_display: String,
    cwd: std::path::PathBuf,
    cancel: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::UnboundedSender<UiEvent>,
    update_tx: tokio::sync::mpsc::UnboundedSender<UpdateNotice>,
    channel_watch_tx: tokio::sync::watch::Sender<flashagent_core::config::UpdateChannel>,
    channel_probe_tx: tokio::sync::mpsc::UnboundedSender<ChannelTarget>,
    update_busy: Arc<AtomicBool>,
    is_discovering: Arc<AtomicBool>,
    started_at: std::time::Instant,
}

impl Wiring {
    fn cx<'a>(
        &'a self,
        rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<UiEvent>,
        session_id: &'a String,
        mascot_mood: MascotMood,
        tip_lines: &'a Vec<String>,
    ) -> LoopCtx<'a> {
        LoopCtx {
            source: &self.source,
            perm: self.perm,
            gate: &self.gate,
            question_gate: &self.question_gate,
            tools_arc: &self.tools_arc,
            memory_block: &self.memory_block,
            cwd_display: &self.cwd_display,
            cwd: &self.cwd,
            cancel: &self.cancel,
            tx: &self.tx,
            rx,
            update_tx: &self.update_tx,
            channel_watch_tx: &self.channel_watch_tx,
            channel_probe_tx: &self.channel_probe_tx,
            update_busy: &self.update_busy,
            is_discovering: &self.is_discovering,
            session_id,
            mascot_mood,
            started_at: self.started_at,
            tip_lines,
        }
    }
}

/// The receiving ends the event loop waits on.
struct Inbox {
    events: tokio::sync::mpsc::UnboundedReceiver<UiEvent>,
    updates: tokio::sync::mpsc::UnboundedReceiver<UpdateNotice>,
    channel_probes: tokio::sync::mpsc::UnboundedReceiver<ChannelTarget>,
}

async fn run_app(ctx: AppContext) -> Result<SaveOutcome> {
    let (tx, events) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    start_event_sources(&tx, &ctx.tools_arc, ctx.pending_discovery);
    let (channel_probe_tx, channel_probes) = tokio::sync::mpsc::unbounded_channel::<ChannelTarget>();
    let (update_tx, updates) = tokio::sync::mpsc::unbounded_channel::<UpdateNotice>();
    let (channel_watch_tx, channel_watch_rx) = tokio::sync::watch::channel(ctx.config.update_channel);
    // The background updater and Ctrl+U both claim this, so they never download
    // over each other.
    let update_busy = Arc::new(AtomicBool::new(false));
    start_background_updates(ctx.config.auto_check_updates, channel_watch_rx, update_tx.clone(), update_busy.clone());
    let w = Wiring {
        source: ctx.source,
        perm: ctx.perm,
        gate: ctx.gate,
        question_gate: ctx.question_gate,
        tools_arc: ctx.tools_arc,
        memory_block: ctx.memory_block,
        cwd_display: ctx.cwd_display,
        cwd: ctx.cwd,
        cancel: Arc::new(AtomicBool::new(false)),
        tx,
        update_tx,
        channel_watch_tx,
        channel_probe_tx,
        update_busy,
        is_discovering: Arc::new(AtomicBool::new(false)),
        started_at: std::time::Instant::now(),
    };

    let system_prompt_config = SystemPromptConfig::new()
        .with_cwd(&w.cwd_display)
        .with_platform(flashagent_tools::shell::platform())
        .with_model(&ctx.model)
        .with_effort(&ctx.initial_effort);
    let system_prompt_text =
        build_system_prompt(&system_prompt_config.clone().with_personality(&ctx.config.personality));
    let (resume_session_id, open_session_picker, session_note) = resolve_session_start(ctx.session_start, &w.cwd_display);
    let mut session_id = resume_session_id.clone().unwrap_or_else(new_session_id);
    open_snapshots(w.perm, &session_id, &w.cwd);

    let effort_memory = flashagent_core::EffortMemory::load();
    w.source.set_effort_bias(effort_memory.steps(&ctx.model));
    w.tools_arc.set_vision_supported(model_sees_images(&w.source, &ctx.model));
    let mut app = initial_app(InitialApp {
        app_config: ctx.config,
        model: ctx.model,
        context_display: ctx.context_display,
        context_capacity: ctx.context_capacity,
        cwd_display: w.cwd_display.clone(),
        initial_effort: ctx.initial_effort,
        available_models: ctx.available_models,
        system_prompt_text,
        system_prompt_config,
        memory_docs: ctx.memory_docs,
        effort_memory,
    });
    update_context_usage(&mut app.context_usage, &app.history, &w.memory_block, &app.chat, w.perm);

    // Before anything is drawn. A resumed session's card is drawn whole: the
    // reveal stops once the transcript has a user message.
    flashagent_tui::anim::set_enabled(app.config.animations);
    let reveal = resume_session_id.is_none() && app.config.animations;
    app.animate_welcome(&w.source, MascotMood::Checking, None, reveal.then_some(1));
    if let Some(verdict) = ctx.first_run_verdict {
        app.chat.push_system(&verdict);
    }
    // The unreadable file keeps its name: saving over it would destroy what may
    // still be recoverable by hand.
    if resume_session_id.as_deref().is_some_and(|id| !app.resume_at_start(id, &w.memory_block, w.perm)) {
        session_id = new_session_id();
        open_snapshots(w.perm, &session_id, &w.cwd);
    }
    if let Some(note) = session_note {
        app.notice(note);
    }
    if open_session_picker {
        app.open_session_picker(&w.cwd_display, &session_id);
    }
    app.warm_prompt_cache(&w.source, w.perm, &w.memory_block);

    event_loop(&mut app, &w, Inbox { events, updates, channel_probes }, &mut session_id).await;

    w.tools_arc.shells().stop_all();
    let saved = app.config.auto_save_sessions && worth_saving(&app.history);
    Ok(saved.then(|| save_on_exit(&session_id, &app.current_model, &w.cwd_display, &app.history)))
}

/// Draws, waits for the next event and hands it to its handler, until one
/// quits.
async fn event_loop(app: &mut App, w: &Wiring, mut inbox: Inbox, session_id: &mut String) {
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(80));
    // Startup has just asked the server; the first poll is not due yet.
    let mut check_interval =
        tokio::time::interval_at(tokio::time::Instant::now() + SERVER_POLL_INTERVAL, SERVER_POLL_INTERVAL);
    check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    const FRAME: std::time::Duration = std::time::Duration::from_millis(16);
    let mut last_draw = w.started_at;
    loop {
        // Switched here, where the session id and the snapshot store live.
        if let Some(id) = app.pending_resume.take().filter(|id| id != session_id) {
            if app.switch_session(session_id, &id, &w.memory_block, w.perm) {
                *session_id = id;
                open_snapshots(w.perm, session_id, &w.cwd);
                app.warm_prompt_cache(&w.source, w.perm, &w.memory_block);
            }
        }
        let mascot_mood = app.server_mood(&w.source, w.started_at);
        let autocomplete = app.autocomplete_popup(&w.gate, &w.question_gate);

        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((100, 24));
        if (term_w, term_h) != app.last_term_size {
            app.last_term_size = (term_w, term_h);
            if !app.chat.has_user_message() {
                app.animate_welcome(&w.source, mascot_mood, Some(term_w as usize), None);
            }
        }
        // Two tip rows only when the window can spare them.
        let tip_rows = if term_h >= 20 { 2 } else { 1 };
        app.tip_animator.fit_to(term_w as usize, tip_rows);
        let tip_lines = app.tip_animator.render_lines(term_w as usize, tip_rows);

        if app.copy_toast.as_ref().is_some_and(|(_, shown)| shown.elapsed().as_secs_f32() >= 2.5) {
            app.copy_toast = None;
        }
        app.announce_mood(mascot_mood, &w.source);

        // A burst of events (a fast stream, a key held down) is drawn once rather
        // than once per event, or the screen falls behind what it shows; never
        // more than a frame late.
        app.tasks_tick(&w.cx(&mut inbox.events, session_id, mascot_mood, &tip_lines));
        if inbox.events.is_empty() || last_draw.elapsed() >= FRAME {
            app.draw(&w.cx(&mut inbox.events, session_id, mascot_mood, &tip_lines), autocomplete.as_ref());
            last_draw = std::time::Instant::now();
        }

        let ev = tokio::select! {
            Some(target) = inbox.channel_probes.recv() => {
                app.on_channel_target(target);
                continue;
            }
            Some(notice) = inbox.updates.recv() => {
                app.on_update_notice(notice);
                continue;
            }
            // Animations run on the clock alone; counting events made spinners race.
            Some(ev) = inbox.events.recv() => ev,
            // While something moves, frames come every 16 ms instead of the idle tick.
            _ = tokio::time::sleep(std::time::Duration::from_millis(16)),
                if app.animating() || (welcome_reveal_rows(w.started_at).is_some() && !app.chat.has_user_message()) =>
            {
                if let Some(rows) = welcome_reveal_rows(w.started_at).filter(|_| !app.running) {
                    app.animate_welcome(&w.source, mascot_mood, None, Some(rows));
                }
                continue;
            }
            _ = tick.tick() => {
                app.on_tick(&w.cx(&mut inbox.events, session_id, mascot_mood, &tip_lines));
                continue;
            }
            _ = check_interval.tick() => {
                app.poll_server(&w.cx(&mut inbox.events, session_id, mascot_mood, &tip_lines));
                continue;
            }
        };
        if let Flow::Quit = app.on_ui_event(&mut w.cx(&mut inbox.events, session_id, mascot_mood, &tip_lines), ev).await {
            return;
        }
    }
}

fn update_context_usage(
    usage: &mut ContextUsage,
    history: &[ChatMessage],
    memory_block: &str,
    chat: &ChatView,
    tools: &dyn flashagent_core::ToolExec,
) {
    let system_chars: usize = history
        .iter()
        .filter(|m| m.role == flashagent_llm::Role::System)
        .map(|m| m.content.len())
        .sum();
    usage.system_tokens = (system_chars / 4).max(150);
    usage.memory_tokens = memory_block.len() / 4;
    // Every advertised schema, MCP included.
    usage.tools_tokens = tools
        .specs()
        .iter()
        .map(|t| (t.name.len() + t.description.len() + t.parameters_json.len()) / 4 + 8)
        .sum();

    let (chat_user, chat_assistant, chat_reasoning, chat_tools) = chat.raw_content_chars();

    let (mut user, mut assistant, mut reasoning, mut tool_out) = (0usize, 0usize, 0usize, 0usize);
    for m in history {
        match m.role {
            flashagent_llm::Role::User => user += m.content.len(),
            flashagent_llm::Role::Assistant => {
                assistant += m.content.len() + m.tool_calls.iter().map(|c| c.args_json.len()).sum::<usize>();
                reasoning += m.reasoning.as_ref().map_or(0, |r| r.len());
            }
            flashagent_llm::Role::Tool => tool_out += m.content.len(),
            flashagent_llm::Role::System => {}
        }
    }
    // Already counted once as memory.
    if !memory_block.is_empty()
        && history.iter().find(|m| m.role == flashagent_llm::Role::User).is_some_and(|m| m.content.starts_with(memory_block))
    {
        user = user.saturating_sub(memory_block.len());
    }

    usage.user_tokens = user.max(chat_user) / 4;
    usage.assistant_tokens = assistant.max(chat_assistant) / 4;
    usage.reasoning_tokens = reasoning.max(chat_reasoning) / 4;
    usage.tool_output_tokens = tool_out.max(chat_tools) / 4;
}

fn build_turn_options(
    app_config: &flashagent_core::AppConfig,
    effort: &str,
) -> flashagent_llm::TurnOptions {
    let mut opts = flashagent_llm::TurnOptions {
        temperature: Some(app_config.temperature),
        top_p: app_config.top_p,
        top_k: app_config.top_k,
        repeat_penalty: app_config.repeat_penalty,
        presence_penalty: app_config.presence_penalty,
        min_p: app_config.min_p,
        ..Default::default()
    };
    if effort == "auto" || effort.is_empty() {
        opts.thinking = flashagent_llm::ThinkingEffort::Auto;
        opts.custom_effort = None;
    } else if effort == "default" {
        // The server's advertised default preset.
        opts.thinking = flashagent_llm::ThinkingEffort::Default;
        opts.custom_effort = None;
    } else {
        opts.custom_effort = Some(effort.to_string());
        match effort.to_lowercase().as_str() {
            "off" => opts.thinking = flashagent_llm::ThinkingEffort::Off,
            "low" => opts.thinking = flashagent_llm::ThinkingEffort::Low,
            "medium" => opts.thinking = flashagent_llm::ThinkingEffort::Medium,
            "high" => opts.thinking = flashagent_llm::ThinkingEffort::High,
            _ => {}
        }
    }
    opts
}

#[allow(clippy::too_many_arguments)]
fn spawn_turn(
    cancel: Arc<std::sync::atomic::AtomicBool>,
    source: Arc<BackendSource>,
    perm: &'static PermissionedTools,
    history: Vec<ChatMessage>,
    budgets: GoalBudgets,
    turn_opts: flashagent_llm::TurnOptions,
    tx: tokio::sync::mpsc::UnboundedSender<UiEvent>,
    steer_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    turn_id: u64,
    voice_prelude: Vec<ChatMessage>,
) -> tokio::task::JoinHandle<()> {
    cancel.store(false, Ordering::Relaxed);
    tokio::spawn(async move {
        let config = LoopConfig {
            max_steps: budgets.steps,
            max_output_tokens: budgets.output_tokens,
            time_budget: budgets.time,
            base_turn_options: turn_opts,
            voice_prelude,
            ..Default::default()
        };
        let loop_ = AgentLoop::with_steering(config, cancel, steer_rx);
        let res = loop_
            .run(source.as_ref(), perm, history, |event| {
                let _ = tx.send(UiEvent::Loop { turn_id, event });
            })
            .await;
        let result = res.map_err(|e| (e.to_string(), e.into_history()));
        let _ = tx.send(UiEvent::Finished { turn_id, result });
    })
}

/// Project `.agents/skills` first, then `~/.flashagent/skills`.
fn find_skill_file(name: &str) -> Option<std::path::PathBuf> {
    if name.is_empty() || name.contains(['/', '\\']) || name.contains("..") {
        return None;
    }
    let file = format!("{name}.md");
    let project = std::path::Path::new(".agents").join("skills").join(&file);
    let global = flashagent_home_dir().map(|h| h.join("skills").join(&file));
    std::iter::once(project).chain(global).find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_that_arrive_together_with_a_newline_inside_are_one_paste() {
        let events = burst_events("fn main() {\n    x\n}");
        assert!(matches!(events.as_slice(), [UiEvent::Paste(p)] if p == "fn main() {\n    x\n}"));
        // A trailing newline: the text, then Enter as a key.
        let events = burst_events("a\nb\n");
        assert!(matches!(events.as_slice(), [UiEvent::Paste(p), UiEvent::Key(KeyCode::Enter, _)] if p == "a\nb"));
    }

    #[test]
    fn a_pasted_tab_keeps_its_line_from_being_sent_on_its_own() {
        // A tab-indented first line with Enter after it was "typed, then sent".
        let events = burst_events("func main() {\n\tfmt.Println(\"hi\")\n");
        assert!(matches!(events.as_slice(), [UiEvent::Paste(p), UiEvent::Key(KeyCode::Enter, _)] if p == "func main() {\n\tfmt.Println(\"hi\")"));
        let events = burst_events("\tfmt.Println(\"hi\")\n}\n");
        assert!(matches!(events.as_slice(), [UiEvent::Paste(_), UiEvent::Key(KeyCode::Enter, _)]));
        assert!(matches!(burst_events("\tfoo()").as_slice(), [UiEvent::Paste(p)] if p == "\tfoo()"));
        assert!(matches!(burst_events("/mo\t").last(), Some(UiEvent::Key(KeyCode::Tab, _))));
        assert!(matches!(burst_events("a\tb").as_slice(), [UiEvent::Paste(p)] if p == "a\tb"));
        // The Tab key alone, its release queued behind it, still opens Settings.
        assert!(matches!(burst_events("\t").as_slice(), [UiEvent::Key(KeyCode::Tab, _)]));
    }

    #[test]
    fn the_editor_setting_that_names_a_variable_is_not_run_as_a_program() {
        assert_ne!(editor_command("$EDITOR"), "$EDITOR");
        assert_ne!(editor_command(""), "");
        assert_eq!(editor_command("code --wait"), "code --wait");
    }

    #[test]
    fn a_word_and_enter_typed_together_stay_keys() {
        let events = burst_events("hi\n");
        assert!(matches!(
            events.as_slice(),
            [UiEvent::Key(KeyCode::Char('h'), _), UiEvent::Key(KeyCode::Char('i'), _), UiEvent::Key(KeyCode::Enter, _)]
        ));
        assert!(matches!(burst_events("\n").as_slice(), [UiEvent::Key(KeyCode::Enter, _)]));
    }

    fn offline_source() -> BackendSource {
        // Nothing listens on port 9; a failed compaction must preserve history.
        BackendSource(flashagent_llm::Client::new(flashagent_llm::Endpoint::detect("http://127.0.0.1:9/v1", None), "m"))
    }

    struct ScriptedCompaction(flashagent_llm::FinishReason);

    #[async_trait::async_trait]
    impl LlmSource for ScriptedCompaction {
        async fn turn(
            &self,
            _messages: &[ChatMessage],
            _tools: &[flashagent_llm::ToolSpec],
        ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
            let events = vec![
                Ok(flashagent_llm::LlmEvent::TextDelta("Summary:\n1. Primary Request and Intent: first question\n7. Pending Tasks: continue the work\n8. Current Work: read a file".into())),
                Ok(flashagent_llm::LlmEvent::Done(self.0)),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn compaction_never_orphans_tool_results_or_stacks_system_messages() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("conversation.jsonl");
        let call = flashagent_llm::ToolCall { id: "c1".into(), name: "read_file".into(), args_json: "{}".into() };
        let mut asst_call = ChatMessage::assistant("");
        asst_call.tool_calls = vec![call];
        let mut history = vec![
            ChatMessage::system("SYSTEM PROMPT"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer".repeat(1000)),
            ChatMessage::user("read a file"),
            asst_call,
            ChatMessage::tool_result("c1", "file body"),
            ChatMessage::assistant("here it is"),
        ];
        let freed = compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, None, &archive).await;
        assert!(freed.is_some());
        assert_eq!(history.iter().filter(|m| m.role == flashagent_llm::Role::System).count(), 1);
        assert!(history[0].content.starts_with("SYSTEM PROMPT"));
        assert!(history[0].content.contains("The summary below covers the earlier conversation.\nSummary:"));
        assert!(history[0].content.contains("Continue the current task from this summary"));
        assert!(history[0].content.contains(&archive.display().to_string()));
        assert!(history[0].content.contains("first question"));
        // The kept region is the whole current turn, from its prompt.
        assert_eq!(history[1].role, flashagent_llm::Role::User);
        assert_eq!(history[1].content, "read a file");
        assert_eq!(history[2].tool_calls.len(), 1);
        assert_eq!(history[3].tool_call_id.as_deref(), Some("c1"));

        // A second compaction keeps the earlier summary.
        history.push(ChatMessage::user("next"));
        history.push(ChatMessage::assistant("ok"));
        history.push(ChatMessage::assistant("more work".repeat(1000)));
        assert!(compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, None, &archive).await.is_some());
        assert!(history[0].content.contains("first question"));
        assert_eq!(history[0].content.matches(COMPACTED_MARK.trim()).count(), 1);
        assert_eq!(history[0].content.matches("The summary below covers the earlier conversation.").count(), 1);
        let archived = std::fs::read_to_string(&archive).unwrap();
        assert!(archived.contains("first answer"));
        assert!(archived.contains("read a file"));
        assert!(archived.contains("file body"));
        let records: Vec<SavedMessage> = archived.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        assert_eq!(records.iter().filter(|m| m.content == "first question").count(), 1);
        assert!(!records.iter().any(|m| m.content == "next"), "the active turn stays live");
    }

    #[tokio::test]
    async fn single_turn_history_is_already_compact() {
        let mut history = vec![ChatMessage::system("s"), ChatMessage::user("u"), ChatMessage::assistant("a")];
        assert_eq!(compact_context(&offline_source(), &mut history, None, std::path::Path::new("unused.jsonl")).await, None);
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    async fn failed_or_truncated_compaction_keeps_the_original_history() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("conversation.jsonl");
        let original = vec![
            ChatMessage::system("system"),
            ChatMessage::user("important instruction"),
            ChatMessage::assistant("work already done".repeat(1000)),
            ChatMessage::user("current task"),
        ];
        for reason in [flashagent_llm::FinishReason::Length, flashagent_llm::FinishReason::ToolUse] {
            let mut history = original.clone();
            assert_eq!(compact_context(&ScriptedCompaction(reason), &mut history, None, &archive).await, None);
            assert_eq!(history.len(), original.len());
            assert_eq!(history[2].content, original[2].content);
        }
        let mut history = original.clone();
        assert_eq!(compact_context(&offline_source(), &mut history, None, &archive).await, None);
        assert_eq!(history[1].content, "important instruction");
        assert!(!archive.exists());

        let blocker = dir.path().join("not_a_directory");
        std::fs::write(&blocker, "file").unwrap();
        let mut history = original.clone();
        assert_eq!(compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, None, &blocker.join("archive.jsonl")).await, None);
        assert_eq!(history[2].content, original[2].content, "archive failure must not discard history");
    }

    #[test]
    fn compaction_input_keeps_goal_instructions_tool_calls_and_results() {
        let mut call = ChatMessage::assistant("");
        call.tool_calls.push(flashagent_llm::ToolCall {
            id: "c1".into(), name: "run_shell".into(), args_json: r#"{"command":"cargo test"}"#.into(),
        });
        let messages = vec![
            ChatMessage::user("memory preamble\n\n---\n\n[AUTONOMOUS GOAL DIRECTIVE]\nTarget goal: ship the release\nBudget: 8 steps"),
            call,
            ChatMessage::tool_result("c1", "tests passed"),
            ChatMessage::tool_result("c2", "x".repeat(50_000)),
        ];
        let transcript = compaction_transcript(&messages, None);
        assert!(transcript.contains("Target goal: ship the release"));
        assert!(transcript.contains("Budget: 8 steps"));
        assert!(transcript.contains("Tool call c1 run_shell: {\"command\":\"cargo test\"}"));
        assert!(transcript.contains("Tool: tests passed"));
        assert!(transcript.contains("Tool result for call: c1"));
        assert!(!transcript.contains("memory preamble"));
        assert!(transcript.len() < 5_000, "a huge tool result is shortened for the summary request");
    }

    #[test]
    fn compaction_summary_needs_goal_pending_work_and_current_state() {
        assert!(!summary_has_handoff_state("Summary:\n1. Primary Request and Intent: do the work"));
        assert!(!summary_has_handoff_state("Summary:\n1. Goal\n7. Pending Tasks"));
        assert!(summary_has_handoff_state("Summary:\n1. Primary Request and Intent: do the work\n7. Pending Tasks: None\n8. Current Work: waiting for a new task"));
    }

    #[test]
    fn steering_injected_event_unpins_pending_steer_and_adds_user_message() {
        let mut chat = ChatView::default();
        chat.push_user("first prompt");
        let mut pending_steers = vec!["please use postgres".to_string()];

        let ev = LoopEvent::SteeringInjected("please use postgres".to_string());
        if let LoopEvent::SteeringInjected(ref directive) = ev {
            if let Some(pos) = pending_steers.iter().position(|s| s == directive) {
                pending_steers.remove(pos);
            }
        }
        chat.on_event(&ev);

        assert!(pending_steers.is_empty(), "pending steers must be unpinned");
        let (settled, live) = chat.render_split(80, false);
        let all: Vec<String> = settled.iter().cloned().chain(live).map(|(_, s)| flashagent_tui::strip_ansi(&s)).collect();
        assert!(all.iter().any(|l| l.contains("please use postgres")), "steer must become a regular user line");
    }

    #[test]
    fn approval_card_shows_commands_whatever_the_json_formatting() {
        let spaced = flashagent_llm::effective_args("{ \"command\" : \"cargo test\" , \"timeout_ms\": 5 }", "run_shell").unwrap();
        assert_eq!(spaced.get("command").and_then(|v| v.as_str()), Some("cargo test"));
        let pythonish = flashagent_llm::effective_args("{'command': 'ls -la'}", "run_shell").unwrap();
        assert_eq!(pythonish.get("command").and_then(|v| v.as_str()), Some("ls -la"));
        // An escape sequence in a command is displayed, never executed.
        assert!(!card_safe("echo \u{1b}[8mhidden").contains('\u{1b}'));
    }

    #[test]
    fn approval_card_never_hides_the_tail_of_a_command() {
        let padded = format!("cargo test{}| sh", " ".repeat(200));
        let rows = card_rows(&padded, 40, 6);
        let joined = rows.join("");
        assert!(joined.contains("| sh"), "{rows:?}");
        assert!(joined.contains("x200"));
        let long = "x".repeat(1000);
        let rows = card_rows(&long, 40, 6);
        assert_eq!(rows.len(), 6);
        assert!(rows[5].contains("more characters"));
    }

    #[test]
    fn default_effort_means_the_server_default_preset() {
        let cfg = AppConfig::default();
        assert_eq!(build_turn_options(&cfg, "default").thinking, flashagent_llm::ThinkingEffort::Default);
        assert_eq!(build_turn_options(&cfg, "auto").thinking, flashagent_llm::ThinkingEffort::Auto);
        let high = build_turn_options(&cfg, "high");
        assert_eq!(high.thinking, flashagent_llm::ThinkingEffort::High);
        assert_eq!(high.custom_effort.as_deref(), Some("high"));
    }

    #[test]
    fn opening_settings_never_persists_the_live_mode_as_default() {
        let persisted = AppConfig { permission_mode: PermissionMode::AcceptEdits, thinking_effort: "auto".into(), ..AppConfig::default() };
        let view = settings_for_runtime(&persisted, PermissionMode::Bypass, "high", "m", &[], 131_072);
        let saved = persisted_from_view(&view.config, &persisted, PermissionMode::Bypass, "high");
        assert_eq!(saved.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(saved.thinking_effort, "auto");
        let mut edited = view.config.clone();
        edited.permission_mode = PermissionMode::Manual;
        let saved = persisted_from_view(&edited, &persisted, PermissionMode::Bypass, "high");
        assert_eq!(saved.permission_mode, PermissionMode::Manual);
    }

    #[test]
    fn settings_open_on_live_session_state() {
        let cfg = AppConfig { permission_mode: PermissionMode::AcceptEdits, thinking_effort: "auto".into(), ..AppConfig::default() };
        let view = settings_for_runtime(&cfg, PermissionMode::Bypass, "high", "gemma", &[], 131_072);
        assert_eq!(view.config.permission_mode, PermissionMode::Bypass);
        assert_eq!(view.config.thinking_effort, "high");
        assert_eq!(view.config.active_profile().model, "gemma");
    }

    #[test]
    fn a_warning_is_only_given_when_the_server_says_the_model_is_blind() {
        use flashagent_llm::{DiscoveredModel, ServerDiscovery, ThinkingProfile};
        let model = |id: &str, vision: bool| DiscoveredModel {
            id: id.to_string(),
            display_name: None,
            is_loaded: true,
            context_length: None,
            max_context_length: None,
            thinking: ThinkingProfile::unsupported(),
            supports_tools: true,
            supports_vision: vision,
        };
        let disc = ServerDiscovery {
            base_url: "http://localhost:1234/v1".into(),
            models: vec![model("sees", true), model("blind", false)],
            active_model: None,
            kind: Default::default(),
        };
        assert!(sees_images(Some(&disc), "sees"));
        assert!(!sees_images(Some(&disc), "blind"));
        assert!(sees_images(Some(&disc), "never-heard-of-it"), "an unknown model is not accused");
        assert!(sees_images(None, "anything"), "nor is one on a server we could not ask");
    }

    #[test]
    fn a_dropped_image_path_becomes_an_attachment() {
        // A dropped picture's path means the picture.
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("shot.png");
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&800u32.to_be_bytes());
        bytes.extend_from_slice(&600u32.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        std::fs::write(&png, &bytes).unwrap();

        let att = Attachment::from_dropped_path(png.to_str().unwrap()).expect("attached");
        assert_eq!(att.name, "shot.png");
        assert_eq!(att.size, Some((800, 600)));
        assert_eq!(att.label(), "shot.png 800×600");
        assert!(att.data_url.starts_with("data:image/png;base64,"));

        // Quoted, as terminals write a path with spaces.
        let quoted = format!("'{}'", png.to_str().unwrap());
        assert!(Attachment::from_dropped_path(&quoted).is_some());
    }

    #[test]
    fn a_bare_file_name_in_a_sentence_is_a_mention_not_an_attachment() {
        // A file named in a sentence is a mention; only a written-out path attaches.
        let looks_like_a_path = |t: &str| t.contains('/') || t.contains('\\');
        assert!(!looks_like_a_path("diagram.png"));
        assert!(looks_like_a_path("./diagram.png"));
        assert!(looks_like_a_path("docs/diagram.png"));
        assert!(looks_like_a_path("/home/me/shot.png"));
    }

    #[test]
    fn ordinary_pasted_text_is_still_text() {
        assert_eq!(Attachment::from_dropped_path("just some words"), None);
        assert_eq!(Attachment::from_dropped_path("/no/such/file.png"), None, "a path to nothing is not a picture");
        assert_eq!(Attachment::from_dropped_path("notes.md"), None);
        assert_eq!(Attachment::from_dropped_path(""), None);
    }

    #[test]
    fn a_resumed_goal_shows_the_goal_not_its_scaffolding() {
        let directive = "[AUTONOMOUS GOAL DIRECTIVE]\nYou are operating in fully autonomous /goal mode.\nTarget goal: Создай файл hello.txt\n\nAutonomous Rules:\n1. Do NOT ask...";
        assert_eq!(extract_user_prompt(directive), "Создай файл hello.txt");
        assert_eq!(extract_user_prompt("plain question"), "plain question");
        assert_eq!(extract_user_prompt("memory block\n\n---\n\nthe question"), "the question");
    }

    #[test]
    fn a_recap_cut_off_by_the_token_limit_is_still_shown() {
        // Observed: the thinking used the whole budget and the JSON never closed.
        let truncated = "{\"recap\": \"Ассистент осмотрел проект и описал его структуру.\", \"suggestion\": \"Покажи код игро";
        let (recap, suggestion) = parse_recap_and_suggestion_json(truncated).expect("recap salvaged");
        assert_eq!(recap, "Ассистент осмотрел проект и описал его структуру.");
        assert_eq!(suggestion, None, "half a suggestion is not a suggestion");
    }

    #[test]
    fn half_a_recap_is_not_shown() {
        let cut_early = "{\"recap\": \"Ассистент осмотр";
        assert_eq!(parse_recap_and_suggestion_json(cut_early), None);
    }

    #[test]
    fn escapes_survive_the_salvage() {
        let truncated = "{\"recap\": \"Read \\\"main.rs\\\" and fixed the loop.\", \"sugg";
        let (recap, _) = parse_recap_and_suggestion_json(truncated).expect("recap salvaged");
        assert_eq!(recap, "Read \"main.rs\" and fixed the loop.");
    }

    #[test]
    fn test_parse_recap_and_suggestion_json() {
        let raw = "```json\n{\"recap\": \"Explained Swift concepts\", \"suggestion\": \"Show mascot code\"}\n```";
        let parsed = parse_recap_and_suggestion_json(raw);
        assert_eq!(
            parsed,
            Some(("Explained Swift concepts".to_string(), Some("Show mascot code".to_string())))
        );

        let generic = "{\"recap\": \"Response generated\", \"suggestion\": \"What's next?\"}";
        assert_eq!(parse_recap_and_suggestion_json(generic), None);
    }

    #[test]
    fn test_sanitize_user_suggestion_conversions() {
        assert_eq!(
            sanitize_user_suggestion("«Tell me about Swift features»"),
            Some("Tell me about Swift features".to_string())
        );
        assert_eq!(
            sanitize_user_suggestion("show code examples of Swift"),
            Some("Show code examples of Swift".to_string())
        );
        assert_eq!(
            sanitize_user_suggestion("tell me about Swift features."),
            Some("Tell me about Swift features".to_string())
        );
        assert_eq!(
            sanitize_user_suggestion("Would you like to know more about memory safety?"),
            Some("Explain more about memory safety".to_string())
        );
        assert_eq!(sanitize_user_suggestion("What's next?"), None);
        assert_eq!(sanitize_user_suggestion("Next"), None);
        assert_eq!(sanitize_user_suggestion("Continue"), None);
    }

    #[test]
    fn suggestions_the_assistant_aimed_at_the_user_are_dropped() {
        // → sends the suggestion, so the assistant asking the user is worse than nothing.
        for bad in [
            "Расскажи о своём проекте и опиши, в чём нужна помощь",
            "Опиши свою задачу подробнее",
            "Напиши, чем могу помочь",
            "Дай знать, если хочешь примеры",
            "Tell me about your project",
            "Let me know what you need",
            "Describe your setup and your goal",
            "Feel free to ask about anything else",
            "Укажи тему для анализа или задай конкретный вопрос по проекту",
            "Specify a topic for analysis or ask a specific question about the project",
            "Specify the task you would like to work on first",
            "Tell me what you need help with",
            "Choose what you want to start with",
            "Напиши, над чем ты хочешь поработать",
            "Уточни, что тебе нужно сделать",
        ] {
            assert_eq!(sanitize_user_suggestion(bad), None, "should have been dropped: {bad}");
        }
    }

    #[test]
    fn an_empty_suggestion_after_small_talk_keeps_the_recap_and_shows_no_suggestion() {
        // An empty suggestion must not cost the recap.
        let (recap, suggestion) = parse_recap_and_suggestion_json(
            r#"{"recap": "The user said hello.", "suggestion": ""}"#,
        )
        .expect("the recap is still valid");
        assert_eq!(recap, "The user said hello.");
        assert_eq!(suggestion, None);
    }

    #[test]
    fn real_follow_up_prompts_still_pass() {
        // The object gives it away, not the verb; these must survive the filter.
        for (input, expected) in [
            ("Расскажи об архитектуре агентного цикла", "Расскажи об архитектуре агентного цикла"),
            ("Объясни, почему тест падает", "Объясни, почему тест падает"),
            ("Запусти тесты и почини то, что упало", "Запусти тесты и почини то, что упало"),
            ("Explain how the permission modes differ", "Explain how the permission modes differ"),
            ("Show the diff before applying it", "Show the diff before applying it"),
            // "you" alone is the assistant and is fine; only its wants are not.
            ("Show what you changed in the parser", "Show what you changed in the parser"),
            ("Explain what you can do with MCP servers", "Explain what you can do with MCP servers"),
            ("Покажи, что ты изменил в парсере", "Покажи, что ты изменил в парсере"),
        ] {
            assert_eq!(
                sanitize_user_suggestion(input).as_deref(),
                Some(expected),
                "should have been kept: {input}"
            );
        }
    }


    #[test]
    fn a_download_in_progress_does_not_fade_in_again_at_every_step() {
        let mut slot = None;
        BackgroundNotice::update_sticky(&mut slot, "Downloading b351 · 10%".into());
        let born = slot.as_ref().unwrap().born;
        std::thread::sleep(std::time::Duration::from_millis(5));
        BackgroundNotice::update_sticky(&mut slot, "Downloading b351 · 20%".into());
        let shown = slot.unwrap();
        assert_eq!(shown.text, "Downloading b351 · 20%");
        assert_eq!(shown.born, born, "the notice started its fade-in over again");
    }

    #[test]
    fn a_shortcut_on_a_russian_layout_is_read_by_its_key() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(latin_shortcut(KeyCode::Char('в'), ctrl), KeyCode::Char('d'));
        assert_eq!(latin_shortcut(KeyCode::Char('Л'), ctrl), KeyCode::Char('K'));
        assert_eq!(latin_shortcut(KeyCode::Char('в'), KeyModifiers::ALT), KeyCode::Char('d'));
        // Typing stays Russian.
        assert_eq!(latin_shortcut(KeyCode::Char('в'), KeyModifiers::NONE), KeyCode::Char('в'));
        assert_eq!(latin_shortcut(KeyCode::Char('в'), KeyModifiers::SHIFT), KeyCode::Char('в'));
        assert_eq!(latin_shortcut(KeyCode::Enter, ctrl), KeyCode::Enter);
    }

    #[test]
    fn the_status_line_names_the_mode_and_what_the_turn_waits_on() {
        let running = format_status_left(true, false, false, None, "Normal", "");
        assert!(running.contains("[Normal]"), "{running}");
        // The phase is in the composer; saying it again here was noise.
        assert!(!running.contains("Generating"), "{running}");
        assert!(!running.contains("Ready"), "{running}");

        let idle = format_status_left(false, false, false, None, "Normal", "");
        assert!(idle.contains("[Normal]") && idle.contains("Ready"), "{idle}");

        let goal = format_status_left(true, true, false, Some("step 12/250 · 4.2k tok · 3m05s/1h0m"), "Autonomous", "");
        assert!(goal.contains("[Goal: Autonomous]"), "{goal}");
        assert!(goal.contains("step 12/250") && goal.contains("3m05s/1h0m"), "{goal}");

        let waiting = format_status_left(true, false, true, None, "Manual", "");
        assert!(waiting.contains("Waiting for your answer"), "{waiting}");
    }

    #[test]
    fn opening_and_closing_saves_nothing() {
        let system_only = vec![ChatMessage::system("you are a helpful agent")];
        assert!(!worth_saving(&system_only), "a start-and-quit must not leave a session file");

        let mut talked = system_only.clone();
        talked.push(ChatMessage::user("привет"));
        assert!(worth_saving(&talked));
    }

    #[test]
    fn the_composer_says_what_the_turn_is_actually_doing() {
        // Every phase is something the loop reported; there is no "almost done".
        assert_eq!(TurnPhase::Waiting.label(), "Waiting for the model");
        assert_eq!(TurnPhase::Thinking.label(), "Thinking");
        assert_eq!(TurnPhase::Writing.label(), "Writing the answer");
        assert_eq!(TurnPhase::AfterTool.label(), "Reading the result");
        assert_eq!(TurnPhase::Stopping.label(), "Stopping");

        // The line above already says what the tool does; repeating it in the
        // composer read as user input.
        assert_eq!(TurnPhase::Tool.label(), "Running a tool");
    }

    #[test]
    fn switching_channel_says_update_or_downgrade_by_version() {
        use flashagent_core::config::UpdateChannel;
        let version = |v: &str| ChannelTarget::Version(v.into());

        let up = channel_switch_warning(UpdateChannel::Stable, "b287", &version("v1.0.0"));
        assert!(up.starts_with("Update: b287 → v1.0.0"), "{up}");
        let down = channel_switch_warning(UpdateChannel::Stable, "b300", &version("v1.0.0+b290"));
        assert!(down.starts_with("Downgrade: b300 → v1.0.0+b290"), "{down}");
        assert!(down.contains("disappear"), "{down}");
        let beta_up = channel_switch_warning(UpdateChannel::Beta, "v1.0.0+b290", &version("b300"));
        assert!(beta_up.starts_with("Update:") && beta_up.contains("regress"), "{beta_up}");
        let beta_down = channel_switch_warning(UpdateChannel::Beta, "v1.0.0", &version("b300"));
        assert!(beta_down.starts_with("Downgrade:"), "{beta_down}");
        assert!(down.ends_with("Continue?") && up.ends_with("Continue?"));

        // Not known yet: no guess either way.
        for unknown in [ChannelTarget::Checking, ChannelTarget::Unknown] {
            let text = channel_switch_warning(UpdateChannel::Stable, "b238", &unknown);
            assert!(text.contains("the newest stable release"), "{text}");
            assert!(!text.starts_with("Update") && !text.starts_with("Downgrade"), "{text}");
        }

        let empty = channel_switch_warning(UpdateChannel::Stable, "b238", &ChannelTarget::Empty);
        assert!(empty.contains("Nothing is published") && empty.contains("stay on b238"), "{empty}");
    }

    #[test]
    fn the_conversation_language_is_named_not_guessed() {
        // Quoted Python is Latin either way.
        let ru = "Покажи пример кода для функции сортировки. Вот пример функции \
                  сортировки пузырьком: def bubble_sort(arr): return sorted(arr)";
        assert_eq!(script_language(ru), Some("Russian"));

        assert_eq!(script_language("Show me a bubble sort in Python, with comments"), None);
        // Too little to judge.
        assert_eq!(script_language("ок"), None);
        assert_eq!(script_language(""), None);
    }

    #[test]
    fn manual_update_progress_shows_what_it_is_doing() {
        use flashagent_svc::updater::UpdateProgress;
        let half = update_progress_line(
            "b235",
            UpdateProgress::Downloading { received: 3_500_000, total: Some(7_000_000) },
        );
        assert!(half.starts_with(UPDATE_LINE_PREFIX), "{half}");
        assert!(half.contains("50%"), "{half}");
        assert!(half.contains("3.3/6.7 MB"), "{half}");
        assert!(half.contains('█') && half.contains('─'), "{half}");

        // No content-length: no fabricated bar.
        let unknown =
            update_progress_line("b235", UpdateProgress::Downloading { received: 1_048_576, total: None });
        assert!(unknown.contains("1.0 MB downloaded"), "{unknown}");
        assert!(!unknown.contains('%'), "{unknown}");

        assert!(update_progress_line("b235", UpdateProgress::Verifying).contains("verifying"));
        assert!(update_progress_line("b235", UpdateProgress::Installing).contains("installing"));
    }

    #[test]
    fn a_resumed_session_gets_the_whole_card_not_a_stuck_reveal() {
        // A resumed session's card must never start truncated.
        let card = flashagent_tui::welcome_card(&flashagent_tui::WelcomeCard {
            model: "m",
            cwd: "/tmp",
            width: 100,
            height: 30,
            ..Default::default()
        });
        assert!(card.len() > 1);
        // A resumed session passes no reveal rows and gets the card as it is.
        let opening = revealed(card.clone(), 1);
        assert_eq!(opening.len(), card.len(), "the card holds its height while it appears");
        assert_eq!(opening[0], card[0], "a fresh start animates it in from the top");
        assert!(opening[1..].iter().all(|(_, text)| text.trim().is_empty()));
    }

    #[test]
    fn test_goal_report_card_states_the_budget_stop() {
        let mut chat = ChatView::default();
        let ledger = GoalLedger::new(
            "refactor the parser".into(),
            GoalBudgets { steps: Some(40), time: None, output_tokens: None },
        );
        push_goal_report(&mut chat, &ledger, DoneReason::StepLimit);
        let rendered = chat.render(100);
        let text: String = rendered
            .iter()
            .map(|(_, t)| flashagent_tui::strip_ansi(t))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Goal report"), "{text}");
        assert!(text.contains("INCOMPLETE"), "{text}");
        assert!(text.contains("step budget reached (40 steps)"), "{text}");

        let widths: Vec<usize> = rendered
            .iter()
            .map(|(_, t)| flashagent_tui::visible_width(&flashagent_tui::strip_ansi(t)))
            .collect();
        assert!(
            widths.windows(2).all(|w| w[0] == w[1]),
            "card rows must line up with the border: {widths:?}"
        );
    }

    #[test]
    fn image_only_submission_formats_cleanly_without_leading_spaces() {
        let att = Attachment {
            name: "screenshot".to_string(),
            data_url: "data:image/png;base64,AAAA".to_string(),
            size: Some((1920, 1080)),
        };
        let labels = [att.label()];
        let text = "";
        let shown = if text.trim().is_empty() {
            format!("[{}]", labels.join(", "))
        } else {
            format!("{text}  [{}]", labels.join(", "))
        };
        assert_eq!(shown, "[screenshot 1920×1080]");
        assert!(!shown.starts_with(' '));
    }
}


