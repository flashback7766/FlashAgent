//! Terminal client binary: wires backend → loop → tools → permissions and
//! renders through `flashagent_tui`. Runs the stack in-process (svc IPC is a
//! later milestone).

use std::io::Write as _;
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
    visible_width, welcome_card, WelcomeCard, AutocompletePopup, ChatView, ConfirmSelect,
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
                    // For this run: saving keeps the URL the config file had.
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
                // Without an id: list this folder's sessions to pick from.
                session_start = match args.next_if(|next| !next.starts_with('-')) {
                    Some(id) => SessionStart::Resume(id),
                    None => SessionStart::Pick,
                };
            }
            "--continue" | "-c" => session_start = SessionStart::Continue,
            "--uninstall" => {
                // `-y` anywhere on the line answers every question with its
                // default, for scripts.
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
    // Carried into the first conversation, because the screen it was printed
    // on is about to be cleared.
    let mut first_run_verdict: Option<String> = None;
    let mut ran_setup = false;
    if (!config.setup_completed || force_setup) && std::io::stdout().is_terminal() {
        let completed = flashagent_tui::run_wizard(&mut config).await.unwrap_or(false);
        if !completed {
            return Ok(());
        }
        // Nothing arrived while they were away: they have been here for a
        // minute. Stamp the version so the next update has something to
        // measure against.
        config.last_seen_version = Some(flashagent_svc::updater::current_version().to_string());
        let _ = config.save();
        ran_setup = true;
    }

    // An update that lands silently is an update nobody uses. Once, after the
    // binary has moved forward, show what arrived.
    if std::io::stdout().is_terminal() {
        let now = flashagent_svc::updater::current_version();
        let news = match config.last_seen_version.as_deref() {
            Some(seen) => flashagent_tui::whatsnew::since(Some(seen), now),
            // Nobody who updated INTO the first build that records a version
            // has one recorded, and they are exactly the people with news to
            // read. A set-up config with no version is an existing user, not
            // a first run — the first run stamps itself before it gets here.
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

    // Spawn server discovery in background immediately so it collects
    // models, context window, and thinking presets concurrently while the user interacts with the startup screen.
    let backend_initial = flashagent_llm::OpenAiCompat::new(&url, &model, api_key.clone());
    let mut discovery_task = tokio::spawn(async move {
        backend_initial.discover_server().await
    });

    let mut cwd = std::env::current_dir()?;

    if config.is_directory_trusted(&cwd) {
        skip_trust = true;
    }

    // Prompt user for directory trust / change working directory / quit
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
        // Whether the chosen model can actually drive tools decides whether
        // anything here works, and finding out by watching it narrate its
        // intentions for ten minutes is a bad first hour. Asked after the
        // trust question, which is instant: nobody should wait a minute for
        // a check and only then be asked where they are.
        first_run_verdict = first_run_tool_check(&config).await;
    }

    // Process-lifetime objects: leaked once, so the spawned loop task can hold
    // &'static references (the process is the session).
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

    // If model was not specified, auto-detect loaded model or first available model
    if model.is_empty() {
        if let Some(ref disc) = discovery {
            if let Some(ref active) = disc.active_model {
                model = active.id.clone();
            } else if let Some(first) = disc.models.first() {
                model = first.id.clone();
            }
        }
    } else if let Some(ref disc) = discovery {
        // If user specified substring/fuzzy model name, align with full ID
        if let Some(matched) = disc.models.iter().find(|m| m.id == model || m.id.contains(&model) || model.contains(&m.id)) {
            model = matched.id.clone();
        }
    }
    backend.set_model(&model);
    // Startup asked the server through another backend; this one sends the
    // turns, and must not wait for its first look to know what was found.
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
        // Only the server saying the model cannot reason means off; saying
        // nothing leaves the model to its own default.
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
    // Files outside the folder FlashAgent was started in are never read or
    // written on a mode's say alone.
    state.set_project_root(cwd.clone());
    // MCP config `read_only`/`read_only_tools` and server readOnlyHint
    // annotations decide what counts as a read for external tools.
    let hint_mgr = mcp_manager.clone();
    state.set_read_only_hint(Arc::new(move |tool: &str| hint_mgr.is_tool_read_only(tool)));
    let tools_arc: Arc<BuiltinTools> = Arc::new(BuiltinTools::new(BuiltinToolsConfig {
        cwd: cwd.clone(),
        // Search goes through DuckDuckGo unless the environment names a
        // Brave key; nothing has to be configured for it to work.
        brave_api_key: std::env::var("BRAVE_API_KEY").ok().filter(|k| !k.trim().is_empty()),
        question_gate: Some(question_gate.clone()),
        is_goal_mode: None,
        toolset_profile: Some(config.toolset_profile),
        web_enabled: Some(config.web_tools),
        context_window: Some(context_capacity),
        mcp_manager: Some(mcp_manager.clone()),
    })?);
    // The parent sees the built-in toolset plus `spawn_agent` (subagents).
    let composite = flashagent_tools::agent_tools(tools_arc.clone(), source.clone(), state.clone());
    let perm: &'static PermissionedTools =
        Box::leak(Box::new(PermissionedTools::new(Arc::new(composite), Some(tools_arc.clone()), state.clone())));

    // Memory injection: project + global docs into the first user message.
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).map(std::path::PathBuf::from).unwrap_or_default();
    let docs = flashagent_core::collect(&cwd, &home.join(".flashagent"));
    let memory_block = flashagent_core::injection_block(&docs, config.token_budget);
    let memory_docs = docs.len();

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
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
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::MoveToColumn(0),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    disable_raw_mode()?;
    if let Ok(Some(ref saved_id)) = result {
        let (term_w, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let card = render_session_saved_card(saved_id, term_w as usize);
        println!();
        for line in card {
            println!("{line}");
        }
        println!();
    }
    if result.is_ok() && UNINSTALL_AFTER_EXIT.load(Ordering::SeqCst) {
        if let Err(e) = flashagent_svc::uninstall::run_interactive(false) {
            eprintln!("Uninstall stopped: {e}");
            std::process::exit(1);
        }
    }
    result.map(|_| ())
}

/// Set by "yes" on the /uninstall card: the app closes and the uninstaller
/// runs once the terminal is back to normal.
pub(crate) static UNINSTALL_AFTER_EXIT: AtomicBool = AtomicBool::new(false);

/// Title of the /uninstall card; also how the hint line knows which card it is.
pub(crate) const UNINSTALL_TITLE: &str = "Uninstall FlashAgent";

/// Which conversation the app opens with.
enum SessionStart {
    /// A new one.
    New,
    /// `--resume <id>`.
    Resume(String),
    /// `--continue`: the newest session of this folder.
    Continue,
    /// `--resume` without an id: this folder's sessions, to pick from.
    Pick,
}

#[derive(Clone)]
struct SavedGoalState {
    mode: PermissionMode,
    effort: String,
    max_steps: Option<u32>,
    task: String,
}

/// What the event loop does after a handler returns.
enum Flow {
    /// Carry on with the rest of this iteration.
    Next,
    /// Start the next iteration.
    Continue,
    /// Leave the loop: the user asked to quit.
    Quit,
}

/// What handlers need from the event loop besides its state: the backend,
/// the tools, and the channels that belong to the loop itself.
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
    /// Set while an update is being checked for or installed.
    update_busy: &'a Arc<AtomicBool>,
    session_id: &'a String,
    /// The mascot's mood this frame.
    mascot_mood: MascotMood,
    /// This frame's tip, already laid out.
    tip_lines: &'a Vec<String>,
}

/// Everything the event loop changes while it runs.
struct App {
    question_ui_state: QuestionUiState,
    history: Vec<ChatMessage>,
    input: String,
    custom_placeholder: Option<String>,
    suggested_prompt: Option<String>,
    latest_suggestion: Option<String>,
    tip_animator: flashagent_tui::tips::TipAnimator,
    input_history: Vec<String>,
    history_index: Option<usize>,
    current_draft: String,
    confirm_select: ConfirmSelect,
    chat: ChatView,
    running: bool,
    active_turn_handle: Option<tokio::task::JoinHandle<()>>,
    active_steer_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
    pending_steers: Vec<String>,
    /// Set when the user interrupts: the loop is asked to stop cooperatively
    /// so it can hand back a consistent history; a hard abort is the fallback.
    cancel_requested: Option<std::time::Instant>,
    aborted_turn: Option<u64>,
    turn_counter: u64,
    all_expanded: bool,
    last_expanded: bool,
    current_model: String,
    current_context: Option<String>,
    current_effort: String,
    /// What the system prompt is built from, so it can be rebuilt when the
    /// user changes how replies should sound.
    prompt_config: SystemPromptConfig,
    /// The folder as shown to the user (`~/project`).
    cwd_display: String,
    /// Rule and memory files loaded into the prompt, for the welcome card.
    memory_docs: usize,
    /// Generation speed sampled a few times a second while a turn runs.
    speed_history: Vec<f64>,
    /// When the last speed sample was taken, on the animation clock.
    last_speed_sample: u64,
    /// How the last turn ended, lit on the composer border: the colour and
    /// when, on the animation clock.
    turn_flash: Option<(flashagent_tui::anim::Rgb, u64)>,
    /// The menu or screen standing in for the composer, if one is open.
    overlay: Option<overlay::Overlay>,
    last_tool_name: Option<String>,
    /// Pictures waiting to go with the next message.
    attachments: Vec<Attachment>,
    /// What a picture costs, measured once per model and remembered.
    image_costs: flashagent_tui::image_cost::ImageCosts,
    image_cost_probe: Option<String>,
    context_usage: ContextUsage,
    autocomplete_idx: usize,
    tick_n: usize,
    renderer: Renderer,
    /// Things that turn up on their own — an update installing, the context
    /// being compacted — go on the line under the input, not in the composer.
    background: Option<BackgroundNotice>,
    channel_switch: Option<ChannelSwitch>,
    /// When Esc was last pressed on an empty prompt: a second press soon
    /// after quits.
    last_esc: Option<std::time::Instant>,
    /// The /uninstall card is up.
    uninstall_confirm: bool,
    /// The recap and suggestion being written for the last turn, so a new
    /// turn can stop it.
    recap_task: Option<tokio::task::JoinHandle<()>>,
    /// A session picked from that list, switched to at the top of the next
    /// loop turn, where the session id and the snapshot store live.
    pending_resume: Option<String>,
    /// The latest stage of an update download, background or manual.
    update_progress: Option<(String, flashagent_svc::updater::UpdateProgress)>,
    /// Whether the user asked to see how an update is going (Ctrl+U,
    /// /update); until then a background update goes unannounced.
    update_watched: bool,
    turn_phase: TurnPhase,
    /// How auto effort's guesses turned out per model, nudged one step at a
    /// time by how its turns actually go.
    effort_memory: flashagent_core::EffortMemory,
    turn_outcome: flashagent_core::TurnOutcome,
    /// How much the last turn added to the context, so the next one can be
    /// sized before it is started rather than after it overflows.
    last_turn_growth: usize,
    context_before_turn: usize,
    pending_update: Option<(String, String, String, Option<String>)>,
    last_term_size: (u16, u16),
    turn_started: Option<std::time::Instant>,
    token_tracker: TokenTracker,
    max_steps: Option<u32>,
    goal_state: Option<SavedGoalState>,
    /// Facts about the running /goal, accumulated from loop events for the
    /// live progress line and the final report.
    goal_ledger: Option<GoalLedger>,
    copy_toast: Option<(String, std::time::Instant)>,
    last_ctrl_c: Option<std::time::Instant>,
    last_mascot_mood: MascotMood,
    /// What the user has already been told about the server, so a mood that
    /// flickers does not re-announce itself.
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
    /// The project directory, where the file tools write.
    cwd: std::path::PathBuf,
    initial_effort: String,
    available_models: Vec<String>,
    session_start: SessionStart,
    /// Verdict of the first-run tool check, to be said in the conversation.
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
    // Let a poll already under way in the reader thread run out first.
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
        // The composer is one line, as a paste is: line breaks become spaces,
        // and the newline editors add at the end goes.
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

async fn run_app(ctx: AppContext) -> Result<Option<String>> {
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

    // Keyboard and mouse reader thread: crossterm is blocking, tokio is async.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
                // While an external editor owns the terminal, its keys are
                // its own: reading here would steal every other one.
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
                    Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                        if tx.send(UiEvent::Key(k.code, k.modifiers)).is_err() {
                            return;
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
        .with_platform(std::env::consts::OS)
        .with_model(&model)
        .with_effort(&initial_effort);
    let system_prompt_text =
        build_system_prompt(&system_prompt_config.clone().with_personality(&app_config.personality));

    let mut tick = tokio::time::interval(std::time::Duration::from_millis(80));
    // The first look is not due yet: startup has just asked the server.
    let mut check_interval =
        tokio::time::interval_at(tokio::time::Instant::now() + SERVER_POLL_INTERVAL, SERVER_POLL_INTERVAL);
    check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let is_discovering = Arc::new(AtomicBool::new(false));
    // --continue opens the newest session of this folder; --resume without an
    // id opens the list of them once the app is up.
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
    // How files were before each turn changed them, kept beside the sessions
    // so /rewind still works after --resume.
    let snapshot_dir = flashagent_home_dir()
        .map(|home| home.join("snapshots").join(&session_id))
        .unwrap_or_else(|| std::env::temp_dir().join("flashagent-snapshots").join(&session_id));
    perm.state().set_snapshots(Arc::new(flashagent_core::SnapshotStore::open(snapshot_dir, cwd.clone())));

    let effort_memory = flashagent_core::EffortMemory::load();
    source.set_effort_bias(effort_memory.steps(&model));
    tools_arc.set_vision_supported(model_sees_images(&source, &model));
    let (channel_probe_tx, mut channel_probe_rx) =
        tokio::sync::mpsc::unbounded_channel::<ChannelTarget>();
    let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<UpdateNotice>();
    let (channel_watch_tx, mut channel_watch_rx) = tokio::sync::watch::channel(app_config.update_channel);
    // One update at a time: the background updater and Ctrl+U both claim
    // this before they start, so they never download over each other.
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
                // An update the user started is under way; look again later.
                if busy.swap(true, Ordering::SeqCst) {
                    continue;
                }
                let progress_tx = update_tx_clone.clone();
                let result = flashagent_svc::updater::check_and_apply_background_with_progress(ch, move |version, stage| {
                    let _ = progress_tx.send(UpdateNotice::Progress { version: version.to_string(), stage });
                })
                .await;
                busy.store(false, Ordering::SeqCst);
                // Up to date and failed are only said to someone watching;
                // the app decides that.
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
    let mut app = App {
        question_ui_state: QuestionUiState::default(),
        history: vec![ChatMessage::system(system_prompt_text)],
        input: String::new(),
        custom_placeholder: None,
        suggested_prompt: None,
        latest_suggestion: None,
        tip_animator: flashagent_tui::tips::TipAnimator::new(),
        input_history: Vec::new(),
        history_index: None,
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
        speed_history: Vec::new(),
        last_speed_sample: 0,
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
        last_esc: None,
        uninstall_confirm: false,
        recap_task: None,
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
        mode: perm.state().mode().label(),
        memory_docs,
        thinking: Some(&thinking_summary),
        context_window: app.current_context.as_deref(),
        width: term_w as usize,
        height: term_h as usize,
        show_mascot: app.config.show_mascot,
        ..WelcomeCard::default()
    });
    // The reveal is driven by the tick loop, which stops touching the card as
    // soon as the transcript has a user message in it. A resumed session puts
    // messages up immediately, so its card would stay stuck at whatever row
    // the reveal had reached — draw it whole instead.
    app.chat.update_welcome_card(opening_card(initial_card, resume_session_id.is_none()));
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
                app.notice(format!("Resumed session '{resume_id}' ({restored} messages loaded)."));
            }
            Err(why) => app.chat.push_line(LineKind::ToolError, why),
        }
    }
    if let Some(note) = session_note {
        app.notice(note);
    }
    if open_session_picker {
        app.open_session_picker(&cwd_display, &session_id);
    }

    macro_rules! finish {
        () => {
            if app.config.auto_save_sessions && worth_saving(&app.history) {
                save_session_file(&session_id, &app.current_model, &cwd_display, &app.history)
                    .map(|_| session_id.clone())
            } else {
                None
            }
        };
    }

    'main_loop: loop {
        // A session picked in /resume: the one open now is saved first, then
        // the picked one takes its place on screen, in the model's history,
        // and in where /rewind keeps its copies.
        if let Some(id) = app.pending_resume.take().filter(|id| *id != session_id) {
            if app.config.auto_save_sessions && worth_saving(&app.history) {
                save_session_file(&session_id, &app.current_model, &cwd_display, &app.history);
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
                    if let Some(home) = flashagent_home_dir() {
                        perm.state().set_snapshots(Arc::new(flashagent_core::SnapshotStore::open(
                            home.join("snapshots").join(&session_id),
                            cwd.clone(),
                        )));
                    }
                    app.latest_suggestion = None;
                    app.renderer.printed_settled = 0;
                    app.renderer.prev_expansion = None;
                    update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
                    app.notice(format!("Resumed session '{session_id}' ({restored} messages loaded)."));
                }
                Err(why) => app.notice(why),
            }
        }
        // The face reports the one thing that decides whether anything works:
        // did the model server answer. Discovery reruns every few seconds, so
        // starting the server later turns the face around on its own.
        // LM Studio on this machine writes what each call really processed to
        // a log the cache figure can be read from; on another host that log
        // is not ours to read.
        app.token_tracker.lm_studio_local = source
            .discovery()
            .is_some_and(|d| d.kind == flashagent_llm::thinking::ServerKind::LmStudio)
            && flashagent_core::url_host(&app.config.backend_url)
                .is_some_and(|h| h == "localhost" || h.starts_with("127.") || h == "::1");
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
                app.animate_welcome(&source, perm.state().mode(), mascot_mood, Some(term_w as usize), None);
            }
        }
        // Two rows for a tip only when the window can spare them.
        let tip_rows = if term_h >= 20 { 2 } else { 1 };
        let tip_lines = app.tip_animator.render_lines(app.tick_n, term_w as usize, tip_rows);
        // What every handler gets from the loop besides its state. A macro
        // rather than a function: it borrows this frame's own values.
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
        // Recomputed every frame: the elapsed part of it moves on its own.
        // The server being unreachable is not something to discover by typing
        // a prompt and waiting. Said once when it happens, taken back when it
        // starts answering.
        if mascot_mood != app.announced_mood {
            let previous = std::mem::replace(&mut app.announced_mood, mascot_mood);
            match mascot_mood {
                MascotMood::Offline => {
                    app.background = Some(BackgroundNotice::sticky(format!(
                        "No model server at {} \u{b7} start it, or pick another in Tab \u{2192} LLM",
                        app.config.backend_url.trim_end_matches('/')
                    )));
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

        {
            let cx = loop_ctx!();
            app.draw(&cx, autocomplete.as_ref());
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
                        // Kept either way, so Ctrl+U can show a download
                        // that started before anyone asked about it.
                        if app.update_watched {
                            app.background = Some(BackgroundNotice::sticky(update_progress_line(&version, stage)));
                        }
                        app.update_progress = Some((version, stage));
                        app.renderer.request_reprint();
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
                            app.background = Some(BackgroundNotice::fading(
                                format!("Update failed: {}", flashagent_tui::truncate_middle(&error, 90)),
                                10,
                            ));
                        }
                    }
                }
                app.renderer.request_reprint();
                continue;
            }
            // Animations run on the clock alone: counting events too made
            // every spinner race whenever the model streamed quickly.
            Some(ev) = rx.recv() => ev,
            // While something is moving (a card unfolding, the welcome card
            // drawing in, the composer flash), frames come every 16 ms rather
            // than on the idle tick.
            _ = tokio::time::sleep(std::time::Duration::from_millis(16)),
                if app.animating() || (welcome_reveal_rows(started_at).is_some() && !app.chat.has_user_message()) =>
            {
                if let Some(rows) = welcome_reveal_rows(started_at).filter(|_| !app.running) {
                    app.animate_welcome(&source, perm.state().mode(), mascot_mood, None, Some(rows));
                }
                continue;
            }
            _ = tick.tick() => {
                app.tick_n += 1;
                app.tip_animator.tick();
                app.sample_speed();
                if app.background.as_ref().is_some_and(BackgroundNotice::expired) {
                    app.background = None;
                    app.renderer.request_reprint();
                }
                // The loop did not wind down in time (a tool ignoring
                // cancellation): abort it. History keeps the prompt but not
                // the partial turn, and the user is told so.
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
                // The mascot breathes and blinks, and the card draws itself
                // in on start-up; rebuild it only on the ticks where it
                // actually looks different.
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
                    app.animate_welcome(&source, perm.state().mode(), mascot_mood, Some(current_term_size.0 as usize), reveal_rows);
                }
                if term_resized {
                    app.renderer.request_reprint();
                }
                continue;
            }
            _ = check_interval.tick() => {
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
                // `turn_id` is the ordinal of the user message the recap is
                // about; regenerate and steering make turn_counter drift.
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
                            let _ = app.config.save();
                            // The effort is the user's choice, not the
                            // server's. A model that cannot reason simply
                            // receives no thinking fields — silently turning
                            // "auto" into "off" here lost the setting for
                            // every model afterwards.
                            if app.current_effort.is_empty() {
                                app.current_effort = "auto".to_string();
                            }
                        }

                        app.refresh_welcome(&source, perm.state().mode(), mascot_mood);
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
            }
            UiEvent::Loop { turn_id, event: e } => {
                // Late events of a turn that was aborted or superseded.
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
                        // Memory is written without being asked, so it is
                        // said out loud. It arrives on its own, which puts it
                        // on the line under the input rather than in the chat.
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
                    app.animate_welcome(&source, perm.state().mode(), mascot_mood, Some(cols as usize), None);
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
                    // A screen that is not a list: the wheel must not scroll
                    // the transcript hidden behind it.
                } else {
                    match m.kind {
                        MouseEventKind::ScrollUp => {
                            app.renderer.scroll_up(3);
                        }
                        MouseEventKind::ScrollDown => {
                            app.renderer.scroll_down(3);
                        }
                        _ => {}
                    }
                }
            }
            UiEvent::Paste(pasted) => {
                // Dropping a file on a terminal pastes its path. When that
                // path is a picture, the user meant the picture.
                if let Some(att) = Attachment::from_dropped_path(&pasted) {
                    let label = att.label();
                    app.attachments.push(att);
                    app.suggested_prompt = None;
                    app.background = Some(BackgroundNotice::fading(
                        format!("{label} attached · Ctrl+Z removes it"),
                        8,
                    ));
                    app.renderer.request_reprint();
                    continue;
                }
                let sanitized = pasted.replace("\r\n", " ").replace(['\n', '\r'], " ");
                if !sanitized.is_empty() {
                    if question_gate.pending().is_some() {
                        app.question_ui_state.write_in_text.push_str(&sanitized);
                    } else if let Some(Overlay::Sampling(sm)) = app.overlay.as_mut() {
                        for ch in sanitized.chars() {
                            sm.handle_key(KeyCode::Char(ch), KeyModifiers::NONE);
                        }
                    } else if app.overlay.is_none() {
                        app.input.push_str(&sanitized);
                        app.history_index = None;
                        app.autocomplete_idx = 0;
                    }
                    app.renderer.request_reprint();
                }
            }
            UiEvent::Key(code, mods) => {
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

    app.renderer.clear_tail();
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
    // What the model is actually sent: every advertised schema (MCP included).
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
    // The memory block rides inside the first user message; it is already
    // counted once as memory.
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
        // The server's own advertised default preset.
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
) -> tokio::task::JoinHandle<()> {
    cancel.store(false, Ordering::Relaxed);
    tokio::spawn(async move {
        let config = LoopConfig {
            max_steps: budgets.steps,
            max_output_tokens: budgets.output_tokens,
            time_budget: budgets.time,
            base_turn_options: turn_opts,
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

/// Skill file for `name`: the project's `.agents/skills` first, then the
/// user's `~/.flashagent/skills` (the same places autocomplete lists).
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

    fn offline_source() -> BackendSource {
        // Nothing listens on port 9: every request fails fast, so compaction
        // takes its offline fallback path.
        BackendSource(flashagent_llm::OpenAiCompat::new("http://127.0.0.1:9/v1", "m", None))
    }

    #[tokio::test]
    async fn compaction_never_orphans_tool_results_or_stacks_system_messages() {
        let call = flashagent_llm::ToolCall { id: "c1".into(), name: "read_file".into(), args_json: "{}".into() };
        let mut asst_call = ChatMessage::assistant("");
        asst_call.tool_calls = vec![call];
        let mut history = vec![
            ChatMessage::system("SYSTEM PROMPT"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
            ChatMessage::user("read a file"),
            asst_call,
            ChatMessage::tool_result("c1", "file body"),
            ChatMessage::assistant("here it is"),
        ];
        let freed = compact_context(&offline_source(), &mut history, None).await;
        assert!(freed.is_some());
        assert_eq!(history.iter().filter(|m| m.role == flashagent_llm::Role::System).count(), 1);
        assert!(history[0].content.starts_with("SYSTEM PROMPT"));
        assert!(history[0].content.contains("first question"));
        // The kept region is the whole current turn, starting at its prompt.
        assert_eq!(history[1].role, flashagent_llm::Role::User);
        assert_eq!(history[1].content, "read a file");
        assert_eq!(history[2].tool_calls.len(), 1);
        assert_eq!(history[3].tool_call_id.as_deref(), Some("c1"));

        // A second compaction keeps the earlier summary instead of dropping it.
        history.push(ChatMessage::user("next"));
        history.push(ChatMessage::assistant("ok"));
        compact_context(&offline_source(), &mut history, None).await;
        assert!(history[0].content.contains("first question"));
        assert_eq!(history[0].content.matches(COMPACTED_MARK.trim()).count(), 1);
    }

    #[tokio::test]
    async fn single_turn_history_is_already_compact() {
        let mut history = vec![ChatMessage::system("s"), ChatMessage::user("u"), ChatMessage::assistant("a")];
        assert_eq!(compact_context(&offline_source(), &mut history, None).await, None);
        assert_eq!(history.len(), 3);
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
        let all: Vec<String> = settled.into_iter().chain(live).map(|(_, s)| flashagent_tui::strip_ansi(&s)).collect();
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
        // Untouched: defaults stay as they were on disk.
        let saved = persisted_from_view(&view.config, &persisted, PermissionMode::Bypass, "high");
        assert_eq!(saved.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(saved.thinking_effort, "auto");
        // Changed in the view: that choice becomes the default.
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
        // Terminals paste the path of a dropped file; a path that points at a
        // picture means the picture.
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

        // Quoted, the way a terminal writes a path with spaces in it.
        let quoted = format!("'{}'", png.to_str().unwrap());
        assert!(Attachment::from_dropped_path(&quoted).is_some());
    }

    #[test]
    fn a_bare_file_name_in_a_sentence_is_a_mention_not_an_attachment() {
        // Attaching on any word ending in .png would send a megabyte because
        // the user said "look at diagram.png"; that is what view_image is
        // for. A written-out path is a different matter.
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
        // Observed for real: the model spent its whole budget thinking and
        // the JSON never closed, so no recap ever appeared. The recap is the
        // first field asked for precisely so it survives this.
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

        // Filters out generic filler
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
        // Generic fillers are rejected
        assert_eq!(sanitize_user_suggestion("What's next?"), None);
        assert_eq!(sanitize_user_suggestion("Next"), None);
        assert_eq!(sanitize_user_suggestion("Continue"), None);
    }

    #[test]
    fn suggestions_the_assistant_aimed_at_the_user_are_dropped() {
        // Pressing → sends the suggestion to the model, so a question the
        // assistant is asking *the user* is worse than no suggestion at all.
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
        // The analyzer is told to leave the suggestion empty when nothing
        // concrete has come up yet; that must not cost the recap.
        let (recap, suggestion) = parse_recap_and_suggestion_json(
            r#"{"recap": "The user said hello.", "suggestion": ""}"#,
        )
        .expect("the recap is still valid");
        assert_eq!(recap, "The user said hello.");
        assert_eq!(suggestion, None);
    }

    #[test]
    fn real_follow_up_prompts_still_pass() {
        // The verb is not the giveaway — the object is. These are things a
        // user genuinely sends next, and they must survive the filter.
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
    fn test_token_tracker_initial_state() {
        let tracker = TokenTracker::new("default".to_string());
        assert_eq!(tracker.format_stats_width(120), None);
    }

    #[test]
    fn test_token_tracker_turn_start_and_usage() {
        let mut tracker = TokenTracker::new("default".to_string());
        tracker.on_turn_start("default".to_string(), 17);

        let stats = tracker.format_stats_width(120).expect("should have stats");
        assert!(stats.contains("17 prompt"));

        // Simulate usage with completion and MTP speculative decoding stats
        let usage = flashagent_llm::Usage {
            prompt: Some(17),
            completion: Some(25),
            cached: None,
            mtp: Some(flashagent_llm::MtpStats {
                total_draft_tokens: 16,
                accepted_draft_tokens: 13,
                rejected_draft_tokens: 3,
            }),
        };
        tracker.on_usage(&usage);
        tracker.last_tg = Some(24.3);
        tracker.on_finished();

        let formatted = tracker.format_stats_width(120).expect("should format stats");
        assert!(formatted.contains("17 prompt"));
        assert!(formatted.contains("24.3 tg"));
        assert!(formatted.contains("mtp: 81%"));
    }

    #[test]
    fn test_token_tracker_mtp_omitted_when_not_applicable() {
        let mut tracker = TokenTracker::new("default".to_string());
        tracker.on_turn_start("default".to_string(), 4920);

        let usage = flashagent_llm::Usage {
            prompt: Some(4920),
            completion: Some(100),
            cached: None,
            mtp: None,
        };
        tracker.on_usage(&usage);
        tracker.last_tg = Some(15.2);
        tracker.on_finished();

        let formatted = tracker.format_stats_width(120).expect("should format stats");
        assert!(formatted.contains("4.9K prompt"));
        assert!(formatted.contains("15.2 tg"));
        assert!(!formatted.contains("mtp:"));
    }

    #[test]
    fn test_format_status_left_running_does_not_duplicate_metrics() {
        let mut tracker = TokenTracker::new("default".to_string());
        tracker.on_turn_start("default".to_string(), 4920);
        let usage = flashagent_llm::Usage {
            prompt: Some(4920),
            completion: Some(100),
            cached: None,
            mtp: None,
        };
        tracker.on_usage(&usage);
        tracker.last_tg = Some(44.4);
        tracker.last_ttft = Some(std::time::Duration::from_millis(1770));

        // When running is true, Line 3 must show "Generating response..." and NOT duplicate prompt tokens or TTFT
        let status = format_status_left(true, false, false, None, "Normal", Some(&tracker), 80, "", 0);
        assert!(status.contains("Generating response..."));
        assert!(status.contains("[Normal]"));
        assert!(!status.contains("4.9K"));
        assert!(!status.contains("TTFT"));
        assert!(!status.contains("44.4 tg"));

        // When running is false, Line 3 displays the completed turn telemetry
        tracker.on_finished();
        let status_done = format_status_left(false, false, false, None, "Normal", Some(&tracker), 80, "", 0);
        assert!(status_done.contains("4.9K prompt"));
        assert!(status_done.contains("TTFT 1.77s"));
        assert!(status_done.contains("44.4 tg"));
        assert!(!status_done.contains("Generating response..."));
    }

    #[test]
    fn test_format_status_left_goal_active_modes() {
        let tracker = TokenTracker::new("default".to_string());
        let status_running = format_status_left(true, true, false, None, "Autonomous", Some(&tracker), 80, "", 0);
        assert!(status_running.contains("[Goal: Autonomous]"));
        assert!(status_running.contains("Generating response..."));

        let status_ready = format_status_left(false, true, false, None, "Autonomous", None, 80, "", 0);
        assert!(status_ready.contains("[Goal: Autonomous]"));
        assert!(status_ready.contains("Ready"));
    }

    #[test]
    fn test_goal_progress_replaces_the_generic_running_text() {
        let tracker = TokenTracker::new("default".to_string());
        let status = format_status_left(
            true,
            true,
            false,
            Some("step 12/250 · 4.2k tok · 3m05s/1h0m"),
            "Autonomous",
            Some(&tracker),
            80,
            "",
            0,
        );
        assert!(status.contains("step 12/250"), "{status}");
        assert!(status.contains("3m05s/1h0m"), "{status}");
        assert!(!status.contains("Generating response..."), "{status}");

        // A turn parked on an approval card is not generating anything.
        let waiting = format_status_left(true, false, true, None, "Manual", Some(&tracker), 80, "", 0);
        assert!(waiting.contains("Waiting for your answer"), "{waiting}");
        assert!(!waiting.contains("Generating response..."), "{waiting}");
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
        // Every one of these comes from something the loop reported. There is
        // deliberately no "almost done": the program does not know that.
        assert_eq!(TurnPhase::Waiting.label(), "Waiting for the model");
        assert_eq!(TurnPhase::Thinking.label(), "Thinking");
        assert_eq!(TurnPhase::Writing.label(), "Writing the answer");
        assert_eq!(TurnPhase::AfterTool.label(), "Reading the result");
        assert_eq!(TurnPhase::Stopping.label(), "Stopping");

        // While a tool runs, the line above the composer already says what
        // the model is doing and why; saying it again inside the composer
        // read as though the user had typed it there.
        assert_eq!(TurnPhase::Tool.label(), "Running a tool");
    }

    #[test]
    fn switching_channel_says_update_or_downgrade_by_version() {
        use flashagent_core::config::UpdateChannel;
        let version = |v: &str| ChannelTarget::Version(v.into());

        // A stable release newer than the beta running is an update...
        let up = channel_switch_warning(UpdateChannel::Stable, "b287", &version("v1.0.0"));
        assert!(up.starts_with("Update: b287 → v1.0.0"), "{up}");
        // ...and one cut from an older build is a downgrade.
        let down = channel_switch_warning(UpdateChannel::Stable, "b300", &version("v1.0.0+b290"));
        assert!(down.starts_with("Downgrade: b300 → v1.0.0+b290"), "{down}");
        assert!(down.contains("disappear"), "{down}");
        // The same rule the other way: a newer beta is an update, with the
        // beta caveat; an older one a downgrade.
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
        // A Russian conversation that quotes Python still reads as Russian:
        // the code is Latin either way.
        let ru = "Покажи пример кода для функции сортировки. Вот пример функции \
                  сортировки пузырьком: def bubble_sort(arr): return sorted(arr)";
        assert_eq!(script_language(ru), Some("Russian"));

        assert_eq!(script_language("Show me a bubble sort in Python, with comments"), None);
        // Too little to judge: better to say nothing than to guess.
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
        assert!(half.contains('█') && half.contains('░'), "{half}");

        // A mirror that sends no content-length must not get a fabricated bar.
        let unknown =
            update_progress_line("b235", UpdateProgress::Downloading { received: 1_048_576, total: None });
        assert!(unknown.contains("1.0 MB downloaded"), "{unknown}");
        assert!(!unknown.contains('%'), "{unknown}");

        // The stages after the download are named, not silent.
        assert!(update_progress_line("b235", UpdateProgress::Verifying).contains("verifying"));
        assert!(update_progress_line("b235", UpdateProgress::Installing).contains("installing"));

        // Every stage rewrites one line rather than stacking up.
        let mut chat = ChatView::default();
        for stage in [
            UpdateProgress::Downloading { received: 1, total: Some(10) },
            UpdateProgress::Downloading { received: 9, total: Some(10) },
            UpdateProgress::Verifying,
            UpdateProgress::Installing,
        ] {
            chat.update_or_push_system(UPDATE_LINE_PREFIX, &update_progress_line("b235", stage));
        }
        let lines = chat.render(120);
        let update_lines = lines
            .iter()
            .filter(|(_, t)| flashagent_tui::strip_ansi(t).trim_start().starts_with(UPDATE_LINE_PREFIX))
            .count();
        assert_eq!(update_lines, 1, "progress must rewrite its line, not stack: {lines:?}");
    }

    #[test]
    fn the_face_keeps_the_user_company_only_while_working() {
        let tracker = TokenTracker::new("default".to_string());
        let working = format_status_left(true, false, false, None, "Normal", Some(&tracker), 80, "", 0);
        assert!(working.contains("(•_•)"), "{working}");
        let blinking = format_status_left(true, false, false, None, "Normal", Some(&tracker), 80, "", 46);
        assert!(blinking.contains("(-_-)"), "{blinking}");
        // Idle the mascot lives on the welcome card; two of them would be one
        // too many.
        let idle = format_status_left(false, false, false, None, "Normal", None, 80, "", 0);
        assert!(!idle.contains("(•_•)"), "{idle}");
    }

    #[test]
    fn a_resumed_session_gets_the_whole_card_not_a_stuck_reveal() {
        // The tick loop stops refreshing the card once the transcript has a
        // user message, so a resumed session must never start truncated.
        let card = flashagent_tui::welcome_card(&flashagent_tui::WelcomeCard {
            model: "m",
            cwd: "/tmp",
            mode: "Manual",
            width: 100,
            height: 30,
            ..Default::default()
        });
        assert!(card.len() > 1);
        assert_eq!(opening_card(card.clone(), false).len(), card.len(), "resume draws it whole");
        assert_eq!(opening_card(card, true).len(), 1, "a fresh start animates it in");
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


