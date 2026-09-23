//! Terminal client: wires backend, loop, tools and permissions in-process and
//! renders through `flashagent_tui`.

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
    visible_width, welcome_card, WelcomeCard, AutocompletePopup, ChatView, ConfirmChoice, ConfirmSelect,
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

#[derive(Default, Clone)]
struct QuestionUiState {
    selected_index: usize,
    write_in_text: String,
    is_writing: bool,
    selected_indices: std::collections::BTreeSet<usize>,
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    let mut config = AppConfig::load();
    let mut force_setup = false;
    let mut skip_trust = false;
    let mut session_start = SessionStart::New;
    let mut tool_test: Option<bool> = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => {
                if let Some(val) = args.next() {
                    // For this run only: saving keeps the URL the config file had.
                    config.url_override = Some((val.clone(), config.backend_url.clone()));
                    config.backend_url = val;
                }
            }
            "--model" => {
                if let Some(val) = args.next() {
                    config.model = val;
                }
            }
            "-v" | "--version" => {
                println!("FlashAgent {}", flashagent_svc::updater::current_version());
                return Ok(());
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
                return Ok(());
            }
            "--update" => {
                if flashagent_svc::updater::is_dev_mode() {
                    println!("In-app updater is disabled in development mode (running from source repository or cargo target build).");
                    println!("To update your dev build, pull latest git commits and run `cargo build --release`.");
                    return Ok(());
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
                return Ok(());
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
                    return Ok(());
                } else {
                    println!("Current channel: {}", config.update_channel.label());
                    return Ok(());
                }
            }
            "--tool-test" => tool_test = Some(false),
            "--all-models" => tool_test = Some(true),
            "--setup" => force_setup = true,
            "-y" | "--yes" => skip_trust = true,
            "-h" | "--help" => {
                println!("FlashAgent TUI\n\nUsage: flashagent [OPTIONS]\n\nOptions:\n  -v, --version        Print version\n  --update             Check and apply updates\n  --channel <name>     Switch release channel (stable, beta)\n  --model <name>       Specify LLM model name\n  --url <endpoint>     API endpoint (default: http://localhost:1234/v1)\n  --setup              Run first-time setup wizard\n  --tool-test [--all-models]  Check whether the model can drive tools\n  -r, --resume [id]    Resume a saved session (without an id: pick one from this folder)\n  -c, --continue       Continue the latest session in this folder\n  -y, --yes            Skip directory trust confirmation\n  --uninstall [-y]     Remove FlashAgent; asks what data to delete (-y: take the defaults)\n  -h, --help           Show this help message");
                return Ok(());
            }
            other => anyhow::bail!("usage: flashagent [-v] [--update] [--channel <stable|beta>] [--model <name>] [--url http://host/v1] [--tool-test [--all-models]] [--setup] [-r|--resume [id]] [-c|--continue] [-y|--yes] (got {other})"),
        }
    }

    if let Some(all_models) = tool_test {
        let code = run_tool_check_cli(&config, all_models).await;
        std::process::exit(code);
    }

    use std::io::IsTerminal;
    // The screens before the chat (setup, release notes, trust) wear the chosen
    // look too.
    flashagent_tui::theme::set(config.color_theme);
    flashagent_tui::anim::set_enabled(config.animations);
    // Carried into the first conversation: the screen it was printed on is about
    // to be cleared.
    let mut first_run_verdict: Option<String> = None;
    let mut ran_setup = false;
    if (!config.setup_completed || force_setup) && std::io::stdout().is_terminal() {
        let completed = flashagent_tui::run_wizard(&mut config).await.unwrap_or(false);
        if !completed {
            return Ok(());
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

    let api_key = config.api_key.clone().or_else(|| std::env::var("FLASHAGENT_API_KEY").ok());
    let url = config.backend_url.clone();
    let mut model = config.model.clone();

    if std::env::var("FLASHAGENT_TRUST_DIR").is_ok() {
        skip_trust = true;
    }

    // Discovery starts now, so models, context window and presets are known by
    // the time the startup screen is done.
    let backend_initial = flashagent_llm::OpenAiCompat::new(&url, &model, api_key.clone());
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
            return Ok(());
        }
        config.trust_directory(&cwd);
        let _ = config.save();
    }

    if ran_setup {
        // Whether the model can drive tools decides whether anything works. Asked
        // after the trust question, which is instant, so nobody waits for a check
        // before being asked where they are.
        first_run_verdict = first_run_tool_check(&config).await;
    }

    // Leaked once so the spawned loop can hold &'static references; the process
    // is the session.
    let backend = flashagent_llm::OpenAiCompat::new(&url, &model, api_key);
    backend.set_max_retries(config.network_retries);

    let startup_timeout = if model.is_empty() {
        std::time::Duration::from_millis(2000)
    } else {
        std::time::Duration::from_millis(500)
    };
    let (discovery, pending_discovery) = match tokio::time::timeout(startup_timeout, &mut discovery_task).await {
        Ok(res) => (res.unwrap_or(None), None),
        Err(_) => (None, Some(discovery_task)),
    };

    // No model given: the loaded one, else the first available.
    if model.is_empty() {
        if let Some(ref disc) = discovery {
            if let Some(ref active) = disc.active_model {
                model = active.id.clone();
            } else if let Some(first) = disc.models.first() {
                model = first.id.clone();
            }
        }
    } else if let Some(ref disc) = discovery {
        // A partial model name is matched to its full id.
        if let Some(matched) = disc.models.iter().find(|m| m.id == model || m.id.contains(&model) || model.contains(&m.id)) {
            model = matched.id.clone();
        }
    }
    backend.set_model(&model);
    // Startup discovered through another backend; this one sends the turns and
    // must know the result from its first request.
    if let Some(ref disc) = discovery {
        backend.adopt_discovery(disc);
    }
    config.model = model.clone();

    if model.is_empty() {
        anyhow::bail!(
            "No model specified and could not connect to LLM server at {url} to auto-detect a loaded model.\n\
             Please start your server (e.g. LM Studio on port 1234) or run with --model <name> or --setup."
        );
    }

    let active_model_info = discovery.as_ref().and_then(|d| {
        d.models.iter().find(|m| m.id == model).cloned()
    });

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
    let home_str = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default();
    let cwd_display = if !home_str.is_empty() {
        cwd.strip_prefix(&home_str)
            .map(|rel| {
                let s = rel.to_string_lossy();
                if s.is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", s.trim_start_matches('/'))
                }
            })
            .unwrap_or_else(|_| cwd.display().to_string())
    } else {
        cwd.display().to_string()
    };

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
    // The built-in toolset plus `spawn_agent`.
    let composite = flashagent_tools::agent_tools(tools_arc.clone(), source.clone(), state.clone());
    let perm: &'static PermissionedTools =
        Box::leak(Box::new(PermissionedTools::new(Arc::new(composite), Some(tools_arc.clone()), state.clone())));

    // Project and global memory docs, injected into the first user message.
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
    let docs = flashagent_core::collect(&cwd, &home.join(".flashagent"));
    let memory_block = flashagent_core::injection_block(&docs, config.token_budget);
    let memory_docs = docs.len();

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

    enable_raw_mode()?;
    let _ = crossterm::execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture,
    );
    let result = run_app(AppContext {
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
    })
    .await;
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
    cancel: &'a Arc<AtomicBool>,
    tx: &'a tokio::sync::mpsc::UnboundedSender<UiEvent>,
    rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<UiEvent>,
    update_tx: &'a tokio::sync::mpsc::UnboundedSender<UpdateNotice>,
    channel_watch_tx: &'a tokio::sync::watch::Sender<flashagent_core::config::UpdateChannel>,
    channel_probe_tx: &'a tokio::sync::mpsc::UnboundedSender<ChannelTarget>,
    /// While an update is being checked or installed.
    update_busy: &'a Arc<AtomicBool>,
    session_id: &'a String,
    mascot_mood: MascotMood,
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
}

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

fn open_in_external_editor(initial_text: &str, preferred_editor: &str) -> std::io::Result<String> {
    let editor = if !preferred_editor.is_empty() {
        preferred_editor.to_string()
    } else {
        std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "nano".to_string())
    };

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
        // The composer is one line: line breaks become spaces, the trailing newline goes.
        Ok(s) if s.success() => std::fs::read_to_string(&temp_file)
            .map(|text| text.trim_end().replace("\r\n", " ").replace(['\n', '\r'], " "))
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
            Ok(Event::Key(k)) => match typed_char(&k) {
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

/// A newline only at the end is a word typed and Enter pressed together.
fn burst_events(text: &str) -> Vec<UiEvent> {
    let body = text.trim_end_matches('\n');
    if body.contains('\n') {
        let mut out = vec![UiEvent::Paste(body.to_string())];
        // The trailing Enters were keys; one may be the user sending.
        out.extend((body.len()..text.len()).map(|_| UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE)));
        return out;
    }
    text.chars()
        .map(|c| match c {
            '\n' => UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE),
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

async fn run_app(ctx: AppContext) -> Result<SaveOutcome> {
    let AppContext {
        config: app_config,
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
        cwd,
        first_run_verdict,
        pending_discovery,
    } = ctx;
    let cancel = Arc::new(AtomicBool::new(false));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();

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
                        let events = match typed_char(&k) {
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

    let system_prompt_config = SystemPromptConfig::new()
        .with_cwd(&cwd_display)
        .with_platform(flashagent_tools::shell::platform())
        .with_model(&model)
        .with_effort(&initial_effort);
    let system_prompt_text =
        build_system_prompt(&system_prompt_config.clone().with_personality(&app_config.personality));

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(80));
    // Startup has just asked the server; the first poll is not due yet.
    let mut check_interval =
        tokio::time::interval_at(tokio::time::Instant::now() + SERVER_POLL_INTERVAL, SERVER_POLL_INTERVAL);
    check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let is_discovering = Arc::new(AtomicBool::new(false));
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
            let newest = sessions_dir().and_then(|dir| sessions_in(&dir, &cwd_display).into_iter().next());
            if newest.is_none() {
                session_note = Some("No saved session in this folder yet; starting a new one.".to_string());
            }
            newest.map(|s| s.id)
        }
    };
    let mut session_id = resume_session_id.clone().unwrap_or_else(new_session_id);
    open_snapshots(perm, &session_id, &cwd);

    let effort_memory = flashagent_core::EffortMemory::load();
    source.set_effort_bias(effort_memory.steps(&model));
    tools_arc.set_vision_supported(model_sees_images(&source, &model));
    let (channel_probe_tx, mut channel_probe_rx) =
        tokio::sync::mpsc::unbounded_channel::<ChannelTarget>();
    let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<UpdateNotice>();
    let (channel_watch_tx, mut channel_watch_rx) = tokio::sync::watch::channel(app_config.update_channel);
    // The background updater and Ctrl+U both claim this, so they never download
    // over each other.
    let update_busy = Arc::new(AtomicBool::new(false));
    if app_config.auto_check_updates && !flashagent_svc::updater::is_dev_mode() {
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

    let started_at = std::time::Instant::now();
    const FRAME: std::time::Duration = std::time::Duration::from_millis(16);
    let mut last_draw = started_at;
    let mut app = App {
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
    };
    update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);

    let (term_w, term_h) = app.last_term_size;
    let thinking_summary = if let Some(ref p) = source.profile() {
        if p.supported && !p.presets.is_empty() {
            format!("{} [{}]", app.current_effort, p.presets.join(", "))
        } else if !p.supported && !p.is_unreported() {
            "disabled (unsupported)".to_string()
        } else {
            app.current_effort.clone()
        }
    } else {
        app.current_effort.clone()
    };
    let initial_card = welcome_card(&WelcomeCard {
        model: &app.current_model,
        cwd: &cwd_display,
        memory_docs,
        thinking: Some(&thinking_summary),
        context_window: app.current_context.as_deref(),
        width: term_w as usize,
        height: term_h as usize,
        show_mascot: app.config.show_mascot,
        ..WelcomeCard::default()
    });
    // Before anything is drawn. A resumed session's card is drawn whole: the
    // reveal stops once the transcript has a user message.
    flashagent_tui::anim::set_enabled(app.config.animations);
    app.chat.update_welcome_card(opening_card(initial_card, resume_session_id.is_none() && app.config.animations));
    if let Some(verdict) = first_run_verdict {
        app.chat.push_system(&verdict);
    }

    if let Some(ref resume_id) = resume_session_id {
        match sessions_dir()
            .ok_or_else(|| "No home directory to read sessions from; starting fresh.".to_string())
            .and_then(|dir| read_session(&dir, resume_id))
        {
            Ok(saved) => {
                let restored = restore_session(saved, &mut app.chat, &mut app.history);
                update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
                app.notice(format!("Resumed session {resume_id} · {}", flashagent_tui::plural(restored, "message", "messages")));
            }
            Err(why) => {
                // The unreadable file keeps its name: saving over it would destroy what may
                // still be recoverable by hand.
                session_id = new_session_id();
                open_snapshots(perm, &session_id, &cwd);
                app.chat.push_line(LineKind::ToolError, why);
            }
        }
    }
    if let Some(note) = session_note {
        app.notice(note);
    }
    if open_session_picker {
        app.open_session_picker(&cwd_display, &session_id);
    }

    app.warm_prompt_cache(&source, perm, &memory_block);

    macro_rules! finish {
        () => {
            if app.config.auto_save_sessions && worth_saving(&app.history) {
                Some(save_on_exit(&session_id, &app.current_model, &cwd_display, &app.history))
            } else {
                None
            }
        };
    }

    'main_loop: loop {
        // The open session is saved first, then the picked one replaces it on
        // screen, in the history and in the snapshot store.
        if let Some(id) = app.pending_resume.take().filter(|id| *id != session_id) {
            if app.config.auto_save_sessions && worth_saving(&app.history) {
                if let Err(why) = save_session_file(&session_id, &app.current_model, &cwd_display, &app.history) {
                    // Switching away would drop the only copy, the one in memory.
                    app.notice(format!("Not switching: this session could not be saved ({why})"));
                    continue;
                }
            }
            match sessions_dir()
                .ok_or_else(|| "No home directory to read sessions from".to_string())
                .and_then(|dir| read_session(&dir, &id))
            {
                Ok(saved) => {
                    let base_system = app
                        .history
                        .first()
                        .map(|m| m.content.split(COMPACTED_MARK).next().unwrap_or_default().to_string())
                        .unwrap_or_default();
                    app.history = vec![ChatMessage::system(base_system)];
                    app.chat.clear();
                    let restored = restore_session(saved, &mut app.chat, &mut app.history);
                    session_id = id;
                    open_snapshots(perm, &session_id, &cwd);
                    app.latest_suggestion = None;
                    update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
                    app.notice(format!("Resumed session {session_id} · {}", flashagent_tui::plural(restored, "message", "messages")));
                    app.warm_prompt_cache(&source, perm, &memory_block);
                }
                Err(why) => app.notice(why),
            }
        }
        // The face shows whether the model server answered. Discovery reruns,
        // so starting the server later turns it around on its own.
        let mascot_mood = if source.discovery().is_some() {
            MascotMood::Happy
        } else if started_at.elapsed() < std::time::Duration::from_secs(5) {
            MascotMood::Checking
        } else {
            MascotMood::Offline
        };
        let autocomplete = if !app.running
            && app.input.starts_with('/')
            && app.overlay.is_none()
            && gate.pending().is_none()
            && question_gate.pending().is_none()
        {
            AutocompletePopup::for_input(&app.input, std::path::Path::new("."), app.autocomplete_idx)
        } else {
            None
        };

        let (term_w, term_h) = crossterm::terminal::size().unwrap_or((100, 24));
        if (term_w, term_h) != app.last_term_size {
            app.last_term_size = (term_w, term_h);
            if !app.chat.has_user_message() {
                app.animate_welcome(&source, mascot_mood, Some(term_w as usize), None);
            }
        }
        // Two tip rows only when the window can spare them.
        let tip_rows = if term_h >= 20 { 2 } else { 1 };
        let tip_lines = app.tip_animator.render_lines(term_w as usize, tip_rows);
        // A macro, not a function: it borrows this frame's own values.
        macro_rules! loop_ctx {
            () => {
                LoopCtx {
                    source: &source,
                    perm,
                    gate: &gate,
                    question_gate: &question_gate,
                    tools_arc: &tools_arc,
                    memory_block: &memory_block,
                    cwd_display: &cwd_display,
                    cancel: &cancel,
                    tx: &tx,
                    rx: &mut rx,
                    update_tx: &update_tx,
                    channel_watch_tx: &channel_watch_tx,
                    channel_probe_tx: &channel_probe_tx,
                    update_busy: &update_busy,
                    session_id: &session_id,
                    mascot_mood,
                    tip_lines: &tip_lines,
                }
            };
        }

        if let Some((_, instant)) = app.copy_toast {
            if instant.elapsed().as_secs_f32() >= 2.5 {
                app.copy_toast = None;
            }
        }
        // An unreachable server is announced once, and taken back when it answers.
        if mascot_mood != app.announced_mood {
            let previous = std::mem::replace(&mut app.announced_mood, mascot_mood);
            match mascot_mood {
                MascotMood::Offline => {
                    app.background = Some(
                        BackgroundNotice::sticky(format!(
                            "No model server at {} \u{b7} start it, or change Backend URL in Tab \u{2192} General",
                            app.config.backend_url.trim_end_matches('/')
                        ))
                        .warning(),
                    );
                }
                MascotMood::Happy if previous == MascotMood::Offline => {
                    app.background = Some(BackgroundNotice::fading(
                        format!("Model server is answering \u{b7} {}", app.current_model),
                        5,
                    ));
                }
                _ => {}
            }
        }

        // A burst of events (a fast stream, a key held down) is drawn once rather
        // than once per event, or the screen falls behind what it shows; never
        // more than a frame late.
        if rx.is_empty() || last_draw.elapsed() >= FRAME {
            let cx = loop_ctx!();
            app.draw(&cx, autocomplete.as_ref());
            last_draw = std::time::Instant::now();
        }

        let ev = tokio::select! {
            Some(target) = channel_probe_rx.recv() => {
                if let Some(sw) = app.channel_switch.as_mut() {
                    sw.target = target;
                    app.renderer.request_reprint();
                }
                continue;
            }
            Some(notice) = update_rx.recv() => {
                match notice {
                    UpdateNotice::Available { version, asset_name, download_url, checksums_url } => {
                        app.pending_update = Some((version.clone(), asset_name, download_url, checksums_url));
                        app.background = Some(BackgroundNotice::sticky(format!(
                            "Update available: {version} · press Ctrl+U to install"
                        )));
                        if let Some(s) = app.settings_view_mut() {
                            s.update_check_status = Some(format!("Available: {version} (Press Ctrl+U)"));
                        }
                    }
                    UpdateNotice::Progress { version, stage } => {
                        // Kept either way, so Ctrl+U can show a download that started unasked.
                        if app.update_watched {
                            BackgroundNotice::update_sticky(&mut app.background, update_progress_line(&version, stage));
                        }
                        app.update_progress = Some((version, stage));
                    }
                    UpdateNotice::Ready { version } => {
                        app.pending_update = None;
                        app.update_progress = None;
                        app.update_watched = false;
                        app.background = Some(BackgroundNotice::sticky(format!(
                            "Updated to {version} \u{b7} restart FlashAgent to use it"
                        )));
                        if let Some(s) = app.settings_view_mut() {
                            s.update_check_status = Some(format!("Ready: {version} (restart to apply)"));
                        }
                    }
                    UpdateNotice::UpToDate { version } => {
                        if let Some(s) = app.settings_view_mut() {
                            s.update_check_status = Some(format!("Up to date ({version})"));
                        }
                        if std::mem::take(&mut app.update_watched) {
                            app.background = Some(BackgroundNotice::fading(
                                format!("FlashAgent {version} is up to date"),
                                6,
                            ));
                        }
                    }
                    UpdateNotice::Failed { error } => {
                        app.update_progress = None;
                        if let Some(s) = app.settings_view_mut() {
                            s.update_check_status = Some(format!("Error: {error}"));
                        }
                        if std::mem::take(&mut app.update_watched) {
                            app.background = Some(
                                BackgroundNotice::fading(
                                    format!("Update failed: {}", flashagent_tui::truncate_middle(&error, 90)),
                                    10,
                                )
                                .warning(),
                            );
                        }
                    }
                }
                app.renderer.request_reprint();
                continue;
            }
            // Animations run on the clock alone; counting events made spinners race.
            Some(ev) = rx.recv() => ev,
            // While something moves, frames come every 16 ms instead of the idle tick.
            _ = tokio::time::sleep(std::time::Duration::from_millis(16)),
                if app.animating() || (welcome_reveal_rows(started_at).is_some() && !app.chat.has_user_message()) =>
            {
                if let Some(rows) = welcome_reveal_rows(started_at).filter(|_| !app.running) {
                    app.animate_welcome(&source, mascot_mood, None, Some(rows));
                }
                continue;
            }
            _ = tick.tick() => {
                app.tick_n += 1;
                app.tip_animator.tick();
                app.start_recap_if_due(&source, &tx);
                if app.background.as_ref().is_some_and(BackgroundNotice::expired) {
                    app.background = None;
                    app.renderer.request_reprint();
                }
                // The loop did not wind down in time (a tool ignoring cancellation). History
                // keeps the prompt but not the partial turn, and the user is told.
                if app.running && app.cancel_requested.is_some_and(|t| t.elapsed() > std::time::Duration::from_secs(3)) {
                    if let Some(handle) = app.active_turn_handle.take() {
                        handle.abort();
                    }
                    app.running = false;
                    app.turn_started = None;
                    app.cancel_requested = None;
                    app.aborted_turn = Some(app.turn_counter);
                    close_dangling_user(&mut app.history, "[turn aborted by the user]");
                    app.token_tracker.on_finished();
                    if let Some(saved) = app.goal_state.take() {
                        tools_arc.set_goal_mode(false);
                        perm.state().set_goal_active(false);
                        perm.state().set_mode(saved.mode);
                        app.current_effort = saved.effort.clone();
                        app.max_steps = saved.max_steps;
                        if let Some(ledger) = app.goal_ledger.take() {
                            push_goal_report(&mut app.chat, &ledger, DoneReason::Cancelled);
                        }
                    }
                    app.chat.on_event(&flashagent_core::LoopEvent::Done(flashagent_core::DoneReason::Cancelled));
                    app.flash_turn_end(Some(DoneReason::Cancelled));
                    app.custom_placeholder = Some("Turn aborted; its partial output was not kept in the model context".to_string());
                    app.renderer.request_reprint();
                }
                let current_term_size = crossterm::terminal::size().unwrap_or((100, 24));
                let term_resized = current_term_size != app.last_term_size;
                if term_resized {
                    app.last_term_size = current_term_size;
                }
                // Rebuilt only on ticks where the card actually looks different.
                let reveal_rows = welcome_reveal_rows(started_at);
                let mood_changed = mascot_mood != app.last_mascot_mood;
                app.last_mascot_mood = mascot_mood;
                if !app.running
                    && !app.chat.has_user_message()
                    && (term_resized
                        || mood_changed
                        || reveal_rows.is_some()
                        || flashagent_tui::mascot_needs_repaint(app.tick_n))
                {
                    app.animate_welcome(&source, mascot_mood, Some(current_term_size.0 as usize), reveal_rows);
                }
                if term_resized {
                    app.renderer.request_reprint();
                }
                continue;
            }
            _ = check_interval.tick() => {
                app.warm_prompt_cache(&source, perm, &memory_block);
                if should_poll_server(app.running, source.0.requests_in_flight(), is_discovering.load(Ordering::Relaxed)) {
                    is_discovering.store(true, Ordering::Relaxed);
                    let source_bg = source.clone();
                    let tx_bg = tx.clone();
                    let flag = is_discovering.clone();
                    tokio::spawn(async move {
                        if let Some(disc) = source_bg.discover_server().await {
                            let _ = tx_bg.send(UiEvent::ServerDiscovered(disc));
                        }
                        flag.store(false, Ordering::Relaxed);
                    });
                }
                continue;
            }
        };

        match ev {
            UiEvent::BackgroundRecap { turn_id, recap, suggestion } => {
                let formatted = format!("  \x1b[38;2;155;165;180mrecap:\x1b[0m \x1b[38;2;225;230;240m{recap}\x1b[0m");
                // The ordinal of the user message the recap is about; regenerate and steering
                // make turn_counter drift.
                let current_turn = app.chat.user_turn_count() as u64;
                if turn_id == current_turn {
                    app.chat.update_or_push_turn_system("recap:", &formatted);
                    app.latest_suggestion = suggestion.clone();
                    app.custom_placeholder = None;
                    if app.input.is_empty() && app.active_turn_handle.is_none() {
                        app.suggested_prompt = suggestion;
                    }
                    app.renderer.request_reprint();
                } else if turn_id < current_turn {
                    app.chat.attach_turn_recap(turn_id, &formatted);
                    app.renderer.request_reprint();
                }
            }
            UiEvent::MemorySummary(result) => {
                if let Ok(summary) = &result {
                    save_summary(summary);
                }
                if let Some(Overlay::Memory(modal)) = app.overlay.as_mut() {
                    modal.summary = match result {
                        Ok(summary) => flashagent_tui::memory_view::SummaryState::Ready(summary),
                        Err(why) => flashagent_tui::memory_view::SummaryState::Failed(why),
                    };
                }
                app.renderer.request_reprint();
            }
            UiEvent::ImageCost { model, per_pixel, fixed } => {
                app.image_costs.set(&model, flashagent_tui::image_cost::ImageCost { per_pixel, fixed });
                app.image_costs.save();
                app.image_cost_probe = None;
                app.renderer.request_reprint();
            }
            UiEvent::ToolTestResult(verdict) => {
                match app.settings_view_mut() {
                    Some(s) => s.tool_test_status = Some(verdict),
                    None => app.notice(format!("Tool test: {verdict}")),
                }
                app.renderer.request_reprint();
            }
            UiEvent::ServerDiscovered(disc) => {
                let is_lm_studio = disc.kind == flashagent_llm::thinking::ServerKind::LmStudio;
                let has_loaded = disc.models.iter().any(|m| m.is_loaded);
                app.available_models = if is_lm_studio && has_loaded {
                    disc.models.iter().filter(|m| m.is_loaded).map(|m| m.id.clone()).collect()
                } else {
                    disc.models.iter().map(|m| m.id.clone()).collect()
                };
                if let Some(active) = disc.active_model {
                    let new_ctx_len = active.context_length.or(active.max_context_length).unwrap_or(131_072);
                    let new_ctx_disp = active.context_display();
                    let model_changed = active.id != app.current_model;
                    let ctx_changed = app.context_usage.total_capacity != new_ctx_len || app.current_context != new_ctx_disp;

                    if model_changed || ctx_changed {
                        let old_m = app.current_model.clone();
                        let old_ctx_len = app.context_usage.total_capacity;
                        app.current_model = active.id.clone();
                        app.current_context = new_ctx_disp;
                        app.context_usage.total_capacity = new_ctx_len.max(1024);
                        tools_arc.set_context_window(Some(new_ctx_len));
                        source.set_model(&app.current_model);
                        source.set_effort_bias(app.effort_memory.steps(&app.current_model));
                        tools_arc.set_vision_supported(model_sees_images(&source, &app.current_model));
                        update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);

                        if model_changed {
                            app.config.model = app.current_model.clone();
                            app.save_config();
                            // The effort is the user's choice. A non-reasoning model just gets no
                            // thinking fields; turning "auto" into "off" here lost it for later models.
                            if app.current_effort.is_empty() {
                                app.current_effort = "auto".to_string();
                            }
                        }

                        app.refresh_welcome(&source, mascot_mood);
                        let ctx_tag = app.current_context.clone().unwrap_or_default();

                        if model_changed {
                            let msg = format!(
                                "Server active model switched: {old_m} -> {}", app.current_model
                            );
                            app.custom_placeholder = Some(msg);
                            app.suggested_prompt = None;
                        } else if ctx_changed {
                            let old_formatted = ContextUsage::format_tokens(old_ctx_len);
                            let new_formatted = ContextUsage::format_tokens(new_ctx_len);
                            let msg = format!(
                                "Model context capacity: {old_formatted} -> {new_formatted} ({ctx_tag})"
                            );
                            app.custom_placeholder = Some(msg);
                            app.suggested_prompt = None;
                        }
                        app.renderer.request_reprint();
                    }
                }
                app.warm_prompt_cache(&source, perm, &memory_block);
            }
            UiEvent::Loop { turn_id, event: e } => {
                // Late events of an aborted or superseded turn.
                if turn_id != app.turn_counter || app.aborted_turn == Some(turn_id) {
                    continue;
                }
                match &e {
                    LoopEvent::TurnDelta(text) => {
                        app.token_tracker.on_delta(text);
                        app.turn_outcome.answer_chars += text.chars().count();
                        app.turn_phase = TurnPhase::Writing;
                    }
                    LoopEvent::ReasoningDelta(text) => {
                        app.token_tracker.on_delta(text);
                        app.turn_outcome.reasoning_chars += text.chars().count();
                        app.turn_phase = TurnPhase::Thinking;
                    }
                    LoopEvent::ToolStarted { name, args_json, .. } => {
                        app.last_tool_name = Some(name.clone());
                        app.turn_outcome.tool_calls += 1;
                        app.token_tracker.on_delta(name);
                        app.token_tracker.on_delta(args_json);
                        app.turn_phase = TurnPhase::Tool;
                    }
                    LoopEvent::ToolFinished { is_error, result, .. } => {
                        if *is_error {
                            app.turn_outcome.failed_tools += 1;
                        }
                        // Memory is written unasked, so it is announced, on the line under the input.
                        let wrote_memory = app.last_tool_name
                            .as_deref()
                            .is_some_and(|n| matches!(n, "memory_create" | "memory_update" | "memory_remove"));
                        if wrote_memory && !*is_error {
                            if let Some(said) = result.as_deref().and_then(|r| r.lines().next()) {
                                app.background = Some(BackgroundNotice::fading(
                                    format!("{said}  ·  /memory to see or change it"),
                                    10,
                                ));
                            }
                        }
                        app.turn_phase = TurnPhase::AfterTool;
                    }
                    LoopEvent::StepStarted { step, .. } if *step > 1 => {
                        app.turn_phase = TurnPhase::AfterTool;
                    }
                    LoopEvent::Usage(u) => {
                        app.token_tracker.on_usage(u);
                    }
                    LoopEvent::SteeringInjected(directive) => {
                        if let Some(pos) = app.pending_steers.iter().position(|s| s == directive) {
                            app.pending_steers.remove(pos);
                        } else if !app.pending_steers.is_empty() {
                            app.pending_steers.remove(0);
                        }
                        app.renderer.request_reprint();
                    }
                    _ => {}
                }
                if let Some(ledger) = app.goal_ledger.as_mut() {
                    let plan_changed = ledger.on_event(&e);
                    if plan_changed {
                        let block = format_plan(ledger.plan());
                        app.chat.update_or_push_turn_system("plan:", &block);
                        app.renderer.request_reprint();
                    }
                    if let LoopEvent::StepStarted { step, .. } = &e {
                        app.renderer.request_reprint();
                        let completed = step.saturating_sub(1);
                        if completed > 0 && completed % MILESTONE_COMMIT_INTERVAL == 0 {
                            if let Some(note) = commit_goal_milestone(ledger, &cwd, completed) {
                                app.chat.push_system(&note);
                                app.renderer.request_reprint();
                            }
                        }
                    }
                }
                app.chat.on_event(&e);
                if app.chat.take_needs_reprint() {
                    app.renderer.request_reprint();
                }
                update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
            }
            UiEvent::Finished { turn_id, result: res } => {
                let mut cx = loop_ctx!();
                match app.finish_turn(&mut cx, turn_id, res).await {
                    Flow::Continue => continue,
                    Flow::Quit => break 'main_loop,
                    Flow::Next => {}
                }
            }
            UiEvent::Resize(cols, rows) => {
                let term_resized = (cols, rows) != app.last_term_size;
                app.last_term_size = (cols, rows);
                if !app.chat.has_user_message() && term_resized {
                    app.animate_welcome(&source, mascot_mood, Some(cols as usize), None);
                }
                app.renderer.request_reprint();
            }
            UiEvent::Mouse(m) => {
                if let Some(menu) = app.overlay.as_mut().and_then(Overlay::select_menu_mut) {
                    match m.kind {
                        MouseEventKind::ScrollUp => menu.up(),
                        MouseEventKind::ScrollDown => menu.down(),
                        _ => {}
                    }
                    app.renderer.request_reprint();
                } else if app.overlay.is_some() {
                    // A screen that is not a list: the wheel must not scroll the transcript behind it.
                } else {
                    match m.kind {
                        MouseEventKind::ScrollUp => {
                            app.renderer.scroll_up(3);
                        }
                        MouseEventKind::ScrollDown => {
                            app.renderer.scroll_down(3);
                        }
                        // A click on a thought or a tool call opens or folds that one alone.
                        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            let expansion = ReasoningExpansion { all: app.all_expanded, last: app.last_expanded };
                            if let Some(row) = app.renderer.chat_row_at(m.row) {
                                app.chat.toggle_row(row, expansion);
                            }
                        }
                        _ => {}
                    }
                }
            }
            UiEvent::Paste(pasted) => {
                // A paste goes to whatever has the keyboard first: a path pasted as a
                // question's answer must not become an attachment.
                let one_line = || pasted.replace("\r\n", " ").replace(['\n', '\r'], " ");
                if let Some(req) = question_gate.pending() {
                    // As typing does: the card turns to the answer of one's own.
                    let state = &mut app.question_ui_state;
                    if let Some(opts) = req.options.as_ref().filter(|_| !state.is_writing) {
                        state.selected_index = opts.len();
                        state.is_writing = true;
                        state.write_in_text.clear();
                    }
                    state.write_in_text.push_str(&one_line());
                } else if let Some(Overlay::Sampling(sm)) = app.overlay.as_mut() {
                    for ch in one_line().chars() {
                        sm.handle_key(KeyCode::Char(ch), KeyModifiers::NONE);
                    }
                } else if app.overlay.is_some() || gate.pending().is_some() {
                    // Nothing there takes text.
                } else if let Some(att) = Attachment::from_dropped_path(&pasted) {
                    // A dropped picture's path means the picture.
                    let label = att.label();
                    app.attachments.push(att);
                    app.suggested_prompt = None;
                    app.background = Some(BackgroundNotice::fading(format!("{label} attached · Ctrl+Z removes it"), 8));
                } else if !pasted.is_empty() {
                    // Pasted code or logs keep their line breaks.
                    app.input.insert_str(&pasted);
                    app.history_index = None;
                    app.autocomplete_idx = 0;
                }
                app.renderer.request_reprint();
            }
            UiEvent::Key(code, mods) => {
                app.postpone_recap();
                let mut cx = loop_ctx!();
                let flow = match app.handle_overlay_key(&mut cx, code, mods).await {
                    Flow::Next => app.handle_key(&mut cx, code, mods).await,
                    claimed => claimed,
                };
                match flow {
                    Flow::Continue => continue,
                    Flow::Quit => break 'main_loop,
                    Flow::Next => {}
                }
            }
        }
    }

    Ok(finish!())
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
        BackendSource(flashagent_llm::OpenAiCompat::new("http://127.0.0.1:9/v1", "m", None))
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
        assert_eq!(view.config.model, "gemma");
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
        assert_eq!(opening_card(card.clone(), false).len(), card.len(), "resume draws it whole");
        let opening = opening_card(card.clone(), true);
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


