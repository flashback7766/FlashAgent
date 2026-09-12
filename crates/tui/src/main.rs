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
use flashagent_tui::goal::{GoalBudgets, GoalLedger};
use flashagent_tui::MascotMood;
use flashagent_tui::{
    clip_ansi, pad_box_row, render_session_saved_card, restore_line_color,
    visible_width, welcome_card_responsive_opts, AutocompletePopup, ChatView, ConfirmSelect,
    ContextModal, LineKind, McpModal, McpModalAction, McpViewTab, PrefillTracker, ReasoningExpansion,
    RenderLine, SamplingAction, SamplingView, SelectItem, SelectMenu, SettingsAction, SettingsView,
    TuiGate, TuiQuestionGate, UiEvent, SPINNER,
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
use render::*;
use tokens::*;
use backend::*;
use menus::*;
use notices::*;
use attachments::*;
use sessions::*;
use recap::*;
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

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = AppConfig::load();
    let mut force_setup = false;
    let mut skip_trust = false;
    let mut resume_session_id = None;
    let mut tool_test: Option<bool> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--url" => {
                if let Some(val) = args.next() {
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
            "--resume" => {
                if let Some(val) = args.next() {
                    resume_session_id = Some(val);
                } else {
                    anyhow::bail!("--resume requires a session ID");
                }
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
                println!("FlashAgent TUI\n\nUsage: flashagent [OPTIONS]\n\nOptions:\n  -v, --version        Print version\n  --update             Check and apply updates\n  --channel <name>     Switch release channel (stable, beta)\n  --model <name>       Specify LLM model name\n  --url <endpoint>     API endpoint (default: http://localhost:1234/v1)\n  --setup              Run first-time setup wizard\n  --tool-test [--all-models]  Check whether the model can drive tools\n  --resume <id>        Resume a previously saved chat session\n  -y, --yes            Skip directory trust confirmation\n  -h, --help           Show this help message");
                return Ok(());
            }
            other => anyhow::bail!("usage: flashagent [-v] [--update] [--channel <stable|beta>] [--model <name>] [--url http://host/v1] [--tool-test [--all-models]] [--setup] [--resume <id>] [-y|--yes] (got {other})"),
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
    let discovery_task = tokio::spawn(async move {
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
    let discovery = discovery_task.await.unwrap_or(None);

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
        if !p.supported {
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
    // MCP config `read_only`/`read_only_tools` and server readOnlyHint
    // annotations decide what counts as a read for external tools.
    let hint_mgr = mcp_manager.clone();
    state.set_read_only_hint(Arc::new(move |tool: &str| hint_mgr.is_tool_read_only(tool)));
    let tools_arc: Arc<BuiltinTools> = Arc::new(BuiltinTools::new(BuiltinToolsConfig {
        cwd: cwd.clone(),
        brave_api_key: None,
        question_gate: Some(question_gate.clone()),
        is_goal_mode: None,
        toolset_profile: Some(config.toolset_profile),
        web_enabled: Some(config.free_search),
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
        resume_session_id,
        first_run_verdict,
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
    result.map(|_| ())
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
    memory_docs: usize,
    cwd_display: &'a String,
    cancel: &'a Arc<AtomicBool>,
    tx: &'a tokio::sync::mpsc::UnboundedSender<UiEvent>,
    rx: &'a mut tokio::sync::mpsc::UnboundedReceiver<UiEvent>,
    update_tx: &'a tokio::sync::mpsc::UnboundedSender<UpdateNotice>,
    channel_watch_tx: &'a tokio::sync::watch::Sender<flashagent_core::config::UpdateChannel>,
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
    cancel_requested: Option<std::time::Instant>,
    aborted_turn: Option<u64>,
    turn_counter: u64,
    all_expanded: bool,
    last_expanded: bool,
    current_model: String,
    current_context: Option<String>,
    current_effort: String,
    effort_menu: Option<SelectMenu<String>>,
    model_menu: Option<SelectMenu<String>>,
    settings_view: Option<SettingsView>,
    sampling_view: Option<SamplingView>,
    context_modal: Option<ContextModal>,
    memory_modal: Option<flashagent_tui::memory_view::MemoryModal>,
    last_tool_name: Option<String>,
    attachments: Vec<Attachment>,
    image_costs: flashagent_tui::image_cost::ImageCosts,
    image_cost_probe: Option<String>,
    mcp_modal: Option<McpModal>,
    context_usage: ContextUsage,
    autocomplete_idx: usize,
    tick_n: usize,
    renderer: Renderer,
    background: Option<BackgroundNotice>,
    channel_switch: Option<ChannelSwitch>,
    quit_confirm: bool,
    turn_phase: TurnPhase,
    effort_memory: flashagent_core::EffortMemory,
    turn_outcome: flashagent_core::TurnOutcome,
    last_turn_growth: usize,
    context_before_turn: usize,
    pending_update: Option<(String, String, String, Option<String>)>,
    last_term_size: (u16, u16),
    turn_started: Option<std::time::Instant>,
    token_tracker: TokenTracker,
    max_steps: Option<u32>,
    goal_state: Option<SavedGoalState>,
    goal_ledger: Option<GoalLedger>,
    copy_toast: Option<(String, std::time::Instant)>,
    last_ctrl_c: Option<std::time::Instant>,
    last_mascot_mood: MascotMood,
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
    initial_effort: String,
    available_models: Vec<String>,
    resume_session_id: Option<String>,
    /// Verdict of the first-run tool check, to be said in the conversation.
    first_run_verdict: Option<String>,
}

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

    let _ = crossterm::terminal::disable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Show,
        crossterm::event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    );

    let status = std::process::Command::new(&editor)
        .arg(&temp_file)
        .status();

    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::cursor::Hide,
        crossterm::terminal::EnterAlternateScreen,
        crossterm::event::EnableMouseCapture
    );

    let result = match status {
        Ok(s) if s.success() => std::fs::read_to_string(&temp_file).unwrap_or_else(|_| initial_text.to_string()),
        _ => initial_text.to_string(),
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
        resume_session_id,
        first_run_verdict,
    } = ctx;
    let cancel = Arc::new(AtomicBool::new(false));
    let question_ui_state = QuestionUiState::default();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();

    // Keyboard and mouse reader thread: crossterm is blocking, tokio is async.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            loop {
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
    let system_prompt_text = build_system_prompt(&system_prompt_config);
    let mut history: Vec<ChatMessage> = vec![ChatMessage::system(system_prompt_text)];
    let input = String::new();
    let mut custom_placeholder: Option<String> = None;
    let mut suggested_prompt: Option<String> = None;


    let latest_suggestion: Option<String> = None;
    let tip_animator = flashagent_tui::tips::TipAnimator::new();
    let input_history: Vec<String> = Vec::new();
    let history_index: Option<usize> = None;
    let current_draft = String::new();
    let confirm_select = ConfirmSelect::new();
    let mut chat = ChatView::default();
    chat.set_language(&app_config.language);
    let running = false;
    let active_turn_handle: Option<tokio::task::JoinHandle<()>> = None;
    let active_steer_tx: Option<tokio::sync::mpsc::UnboundedSender<String>> = None;
    // Set when the user interrupts: the loop is asked to stop cooperatively so
    // it can hand back a consistent history; a hard abort is the fallback.
    let cancel_requested: Option<std::time::Instant> = None;
    let aborted_turn: Option<u64> = None;
    let turn_counter: u64 = 0;
    let all_expanded = false;
    let last_expanded = false;
    let current_model = model;
    let current_context = context_display;
    let current_effort = initial_effort;
    let effort_menu: Option<SelectMenu<String>> = None;
    let model_menu: Option<SelectMenu<String>> = None;
    let settings_view: Option<SettingsView> = None;
    let sampling_view: Option<SamplingView> = None;
    let context_modal: Option<ContextModal> = None;
    let memory_modal: Option<flashagent_tui::memory_view::MemoryModal> = None;
    let last_tool_name: Option<String> = None;
    // Pictures waiting to go with the next message. A screenshot is the
    // fastest way to say "this is what I mean", and typing it out is the
    // slowest.
    let attachments: Vec<Attachment> = Vec::new();
    // What a picture costs is measured once per model and remembered.
    let image_costs = flashagent_tui::image_cost::ImageCosts::load();
    let image_cost_probe: Option<String> = None;
    let mcp_modal: Option<McpModal> = None;
    let mut context_usage = ContextUsage::new(context_capacity);
    update_context_usage(&mut context_usage, &history, &memory_block, &chat, perm);
    let autocomplete_idx = 0usize;
    let tick_n = 0usize;
    let mut renderer = Renderer::new();

    /// A one-line system notice. These go under the cursor rather than into
    /// the transcript: they are addressed to the person at the keyboard, not
    /// to the conversation, and a chat full of "[No models discovered]" is a
    /// chat you stop reading. Anything with structure — a listing, a diff, a
    /// report — still belongs in the transcript.
    macro_rules! notice {
        ($text:expr) => {{
            custom_placeholder = Some(($text).to_string());
            // A pending suggestion is drawn in the same spot and would hide
            // the notice; take() rather than assign, so a site that sets it
            // again right after is not flagged as a dead store.
            suggested_prompt.take();
            renderer.request_reprint();
        }};
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(80));
    let mut check_interval = tokio::time::interval(std::time::Duration::from_secs(3));
    check_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let is_discovering = Arc::new(AtomicBool::new(false));
    let session_id = resume_session_id.clone().unwrap_or_else(|| {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        format!("session_{ts}")
    });

    // Things that turn up on their own — an update installing, the context
    // being compacted — go on the line under the input. The composer is where
    // the user's own actions are answered; a message they did not ask for
    // must not take that spot.
    let background: Option<BackgroundNotice> = None;
    let channel_switch: Option<ChannelSwitch> = None;
    // Esc used to quit outright, which loses a session to one stray keypress.
    let quit_confirm = false;
    let turn_phase = TurnPhase::Waiting;
    // Auto effort guesses the task before the model has said a word. How its
    // turns actually go is the only evidence of whether the guess fits this
    // model, so the turns are watched and the guess is nudged by one step.
    let effort_memory = flashagent_core::EffortMemory::load();
    let turn_outcome = flashagent_core::TurnOutcome::default();
    // How much the last turn added to the context, so the next one can be
    // sized before it is started rather than after it overflows.
    let last_turn_growth: usize = 0;
    let context_before_turn: usize = 0;
    source.set_effort_bias(effort_memory.steps(&current_model));
    tools_arc.set_vision_supported(model_sees_images(&source, &current_model));
    let (channel_probe_tx, mut channel_probe_rx) =
        tokio::sync::mpsc::unbounded_channel::<ChannelTarget>();
    let pending_update: Option<(String, String, String, Option<String>)> = None;
    let (update_tx, mut update_rx) = tokio::sync::mpsc::unbounded_channel::<UpdateNotice>();
    let (channel_watch_tx, mut channel_watch_rx) = tokio::sync::watch::channel(app_config.update_channel);
    if app_config.auto_check_updates && !flashagent_svc::updater::is_dev_mode() {
        let update_tx_clone = update_tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(flashagent_svc::updater::BACKGROUND_UPDATE_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let ch = *channel_watch_rx.borrow();
                        if let Ok(Some(target_ver)) = flashagent_svc::updater::check_and_apply_background(ch).await {
                            let _ = update_tx_clone.send(UpdateNotice::Ready { version: target_ver });
                            break;
                        }
                    }
                    changed = channel_watch_rx.changed() => {
                        if changed.is_ok() {
                            let ch = *channel_watch_rx.borrow();
                            interval.reset();
                            if let Ok(Some(target_ver)) = flashagent_svc::updater::check_and_apply_background(ch).await {
                                let _ = update_tx_clone.send(UpdateNotice::Ready { version: target_ver });
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        });
    } else if app_config.silent_update_check && !flashagent_svc::updater::is_dev_mode() {
        let silent_tx_clone = update_tx.clone();
        let ch = app_config.update_channel;
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(4)).await;
            if let Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) =
                flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await
            {
                let _ = silent_tx_clone.send(UpdateNotice::Available { version: target, asset_name, download_url, checksums_url });
            }
        });
    }

    let (term_w, term_h) = crossterm::terminal::size().unwrap_or((100, 24));
    let last_term_size = (term_w, term_h);
    let thinking_summary = if let Some(ref p) = source.profile() {
        if p.supported && !p.presets.is_empty() {
            format!("{} [{}]", current_effort, p.presets.join(", "))
        } else if !p.supported {
            "disabled (unsupported)".to_string()
        } else {
            current_effort.clone()
        }
    } else {
        current_effort.clone()
    };
    let initial_card = welcome_card_responsive_opts(
        &current_model,
        &cwd_display,
        perm.state().mode().label(),
        memory_docs,
        Some(&thinking_summary),
        current_context.as_deref(),
        term_w as usize,
        term_h as usize,
        0,
        app_config.show_mascot,
        MascotMood::Checking,
    );
    // The reveal is driven by the tick loop, which stops touching the card as
    // soon as the transcript has a user message in it. A resumed session puts
    // messages up immediately, so its card would stay stuck at whatever row
    // the reveal had reached — draw it whole instead.
    chat.update_welcome_card(opening_card(initial_card, resume_session_id.is_none()));
    if let Some(verdict) = first_run_verdict {
        chat.push_system(&verdict);
    }

    if let Some(ref resume_id) = resume_session_id {
        if let Some(home) = flashagent_home_dir() {
            let session_path = home.join("sessions").join(format!("{resume_id}.json"));
            if let Ok(content) = std::fs::read_to_string(&session_path) {
                if let Ok(saved) = serde_json::from_str::<SavedSession>(&content) {
                    for saved_msg in saved.messages {
                        let msg: ChatMessage = saved_msg.into();
                        match msg.role {
                            flashagent_llm::Role::User => {
                                chat.push_user(extract_user_prompt(&msg.content));
                                history.push(msg);
                            }
                            flashagent_llm::Role::Assistant => {
                                if !msg.content.trim().is_empty() {
                                    chat.push_assistant(&msg.content);
                                }
                                history.push(msg);
                            }
                            // The fresh system prompt (current cwd/model) wins;
                            // only a compaction summary carries over. A second
                            // system message mid-history breaks strict chat
                            // templates (Gemma, Qwen).
                            flashagent_llm::Role::System => {
                                // Current sessions keep the summary inside the
                                // system prompt; older ones stored it as its own
                                // system message. Both carry the marker title.
                                let title = COMPACTED_MARK.trim_start();
                                if let Some(pos) = msg.content.find(title) {
                                    history[0].content.push_str(COMPACTED_MARK);
                                    history[0].content.push_str(msg.content[pos + title.len()..].trim_start());
                                }
                            }
                            flashagent_llm::Role::Tool => history.push(msg),
                        }
                    }
                    notice!(&format!("Resumed session '{resume_id}' ({} messages loaded).", history.len()));
                } else {
                    chat.push_line(LineKind::ToolError, format!("Session file {} is unreadable; starting fresh.", session_path.display()));
                }
            } else {
                chat.push_line(LineKind::ToolError, format!("No saved session '{resume_id}' in ~/.flashagent/sessions; starting fresh."));
            }
        }
    }

    let turn_started: Option<std::time::Instant> = None;
    let token_tracker = TokenTracker::new(current_model.clone());
    let max_steps: Option<u32> = app_config.max_steps;
    let goal_state: Option<SavedGoalState> = None;
    // Facts about the running /goal, accumulated from loop events for the
    // live progress line and the final report.
    let goal_ledger: Option<GoalLedger> = None;
    let copy_toast: Option<(String, std::time::Instant)> = None;
    let last_ctrl_c: Option<std::time::Instant> = None;
    let started_at = std::time::Instant::now();
    let last_mascot_mood = MascotMood::Checking;
    // What the user has already been told about the server, so a mood that
    // flickers does not re-announce itself.
    let announced_mood = MascotMood::Checking;


    let mut app = App {
        question_ui_state,
        history,
        input,
        custom_placeholder,
        suggested_prompt,
        latest_suggestion,
        tip_animator,
        input_history,
        history_index,
        current_draft,
        confirm_select,
        chat,
        running,
        active_turn_handle,
        active_steer_tx,
        cancel_requested,
        aborted_turn,
        turn_counter,
        all_expanded,
        last_expanded,
        current_model,
        current_context,
        current_effort,
        effort_menu,
        model_menu,
        settings_view,
        sampling_view,
        context_modal,
        memory_modal,
        last_tool_name,
        attachments,
        image_costs,
        image_cost_probe,
        mcp_modal,
        context_usage,
        autocomplete_idx,
        tick_n,
        renderer,
        background,
        channel_switch,
        quit_confirm,
        turn_phase,
        effort_memory,
        turn_outcome,
        last_turn_growth,
        context_before_turn,
        pending_update,
        last_term_size,
        turn_started,
        token_tracker,
        max_steps,
        goal_state,
        goal_ledger,
        copy_toast,
        last_ctrl_c,
        last_mascot_mood,
        announced_mood,
        config: app_config,
        available_models,
    };

    macro_rules! notice {
        ($text:expr) => {{
            app.custom_placeholder = Some(($text).to_string());
            app.suggested_prompt.take();
            app.renderer.request_reprint();
        }};
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
        // The face reports the one thing that decides whether anything works:
        // did the model server answer. Discovery reruns every few seconds, so
        // starting the server later turns the face around on its own.
        let mascot_mood = if source.discovery().is_some() {
            MascotMood::Happy
        } else if started_at.elapsed() < std::time::Duration::from_secs(5) {
            MascotMood::Checking
        } else {
            MascotMood::Offline
        };
        let autocomplete = if !app.running
            && app.input.starts_with('/')
            && app.effort_menu.is_none()
            && app.model_menu.is_none()
            && app.settings_view.is_none()
            && app.sampling_view.is_none()
            && app.context_modal.is_none()
            && app.mcp_modal.is_none()
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
                refresh_welcome_card_animated(
                    &mut app.chat,
                    &mut app.renderer,
                    &app.current_model,
                    &cwd_display,
                    perm.state().mode().label(),
                    memory_docs,
                    &source,
                    &app.current_effort,
                    app.current_context.as_deref(),
                    app.tick_n,
                    Some(term_w as usize),
                    app.config.show_mascot,
                    mascot_mood,
                    None,
                );
            }
        }
        let tip_lines = app.tip_animator.render_lines(app.tick_n, term_w as usize);

        if let Some((_, instant)) = app.copy_toast {
            if instant.elapsed().as_secs_f32() >= 2.5 {
                app.copy_toast = None;
            }
        }
        let active_toast = app.copy_toast.as_ref().map(|(msg, _)| msg.as_str());
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

        let channel_prompt: Option<String> = app.channel_switch.as_ref().map(|sw| {
            channel_switch_warning(sw.to, flashagent_svc::updater::current_version(), &sw.target)
        });
        let cost_now = app.image_costs.get(&app.current_model);
        let attachment_labels: Vec<String> =
            app.attachments.iter().map(|a| a.labelled(cost_now)).collect();
        let quit_prompt: Option<&str> = app.quit_confirm.then_some(
            "Quit FlashAgent? The session is saved either way and comes back with --resume.",
        );
        let goal_progress: Option<String> =
            app.goal_ledger.as_ref().filter(|_| app.running).map(|l| l.progress());
        let live_prefill = app.token_tracker.live_prefill_status();
        let ttft_display = app.token_tracker.ttft_display();
        let tg_speed = app.token_tracker.tg_3s();

        app.renderer.frame(
            &app.chat,
            &gate,
            &question_gate,
            app.effort_menu.as_ref(),
            app.model_menu.as_ref(),
            app.settings_view.as_ref(),
            app.sampling_view.as_ref(),
            app.context_modal.as_ref(),
            app.mcp_modal.as_ref(),
            app.memory_modal.as_ref(),
            autocomplete.as_ref(),
            &app.context_usage,
            FrameState {
                input: &app.input,
                mode: perm.state().mode(),
                is_goal_active: app.goal_state.is_some(),
                goal_progress: goal_progress.as_deref(),
                tip: if app.config.show_tips { Some(app.tip_animator.tip_text) } else { None },
                tip_animated: None,
                tip_lines: if app.config.show_tips { Some(&tip_lines) } else { None },
                token_tracker: if app.config.show_tokens { Some(&app.token_tracker) } else { None },
                reasoning_expand: ReasoningExpansion {
                    all: app.all_expanded,
                    last: app.last_expanded,
                },
                tick_n: app.tick_n,
                running: app.running,
                elapsed_secs: app.turn_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                face_phase: app.turn_started.map(|t| (t.elapsed().as_millis() / 80) as usize).unwrap_or(0),
                model_tokens: if app.config.show_tokens { app.token_tracker.total_model_tokens } else { 0 },
                tokens_per_sec: if app.config.show_tokens { tg_speed } else { 0.0 },
                f_keep: if app.config.show_tokens { app.token_tracker.last_f_keep } else { None },
                confirm_selection: app.confirm_select.decision(),
                question_state: Some(&app.question_ui_state),
                custom_placeholder: app.custom_placeholder.as_deref(),
                suggested_prompt: app.suggested_prompt.as_deref(),
                copy_toast: if app.config.show_toasts { active_toast } else { None },
                prefill_status: if app.config.show_ttft { live_prefill.as_deref() } else { None },
                ttft_display: if app.config.show_ttft { ttft_display.as_deref() } else { None },
                background: app.background.as_ref().map(|b| b.text.as_str()),
                channel_prompt: channel_prompt.as_deref(),
                quit_prompt,
                turn_phase: app.running.then_some(&app.turn_phase),
                attachments: &attachment_labels,
                background_style: app.background.as_ref().map_or(NoticeStyle::FULL, BackgroundNotice::style),
                context_warn_threshold: app.config.context_warn_threshold,
            },
        );

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
                        if let Some(ref mut s) = app.settings_view {
                            s.update_check_status = Some(format!("Available: {version} (Press Ctrl+U)"));
                        }
                    }
                    UpdateNotice::Progress { version, stage } => {
                        app.background = Some(BackgroundNotice::sticky(update_progress_line(&version, stage)));
                        app.renderer.request_reprint();
                    }
                    UpdateNotice::Ready { version } => {
                        app.pending_update = None;
                        app.background = Some(BackgroundNotice::sticky(format!(
                            "Update ready: {version} · restart FlashAgent to run it"
                        )));
                        if let Some(ref mut s) = app.settings_view {
                            s.update_check_status = Some(format!("Ready: {version} (restart to apply)"));
                        }
                    }
                    UpdateNotice::UpToDate { version } => {
                        if let Some(ref mut s) = app.settings_view {
                            s.update_check_status = Some(format!("Up to date ({version})"));
                        }
                        app.background = Some(BackgroundNotice::fading(
                            format!("FlashAgent {version} is up to date"),
                            6,
                        ));
                    }
                    UpdateNotice::Failed { error } => {
                        if let Some(ref mut s) = app.settings_view {
                            s.update_check_status = Some(format!("Error: {error}"));
                        }
                        app.background = Some(BackgroundNotice::fading(
                            format!("Update failed: {}", flashagent_tui::truncate_middle(&error, 90)),
                            10,
                        ));
                    }
                }
                app.renderer.request_reprint();
                continue;
            }
            Some(ev) = rx.recv() => {
                app.tick_n += 1;
                ev
            }
            _ = tick.tick() => {
                app.tick_n += 1;
                app.tip_animator.tick();
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
                        perm.state().set_mode(saved.mode);
                        app.current_effort = saved.effort.clone();
                        app.max_steps = saved.max_steps;
                        if let Some(ledger) = app.goal_ledger.take() {
                            push_goal_report(&mut app.chat, &ledger, DoneReason::Cancelled);
                        }
                    }
                    app.chat.on_event(&flashagent_core::LoopEvent::Done(flashagent_core::DoneReason::Cancelled));
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
                    refresh_welcome_card_animated(
                        &mut app.chat,
                        &mut app.renderer,
                        &app.current_model,
                        &cwd_display,
                        perm.state().mode().label(),
                        memory_docs,
                        &source,
                        &app.current_effort,
                        app.current_context.as_deref(),
                        app.tick_n,
                        Some(current_term_size.0 as usize),
                        app.config.show_mascot,
                        mascot_mood,
                        reveal_rows,
                    );
                }
                if term_resized {
                    app.renderer.request_reprint();
                }
                continue;
            }
            _ = check_interval.tick() => {
                if !app.running && !is_discovering.load(Ordering::Relaxed) {
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
            UiEvent::ImageCost { model, per_pixel, fixed } => {
                app.image_costs.set(&model, flashagent_tui::image_cost::ImageCost { per_pixel, fixed });
                app.image_costs.save();
                app.image_cost_probe = None;
                app.renderer.request_reprint();
            }
            UiEvent::ToolTestResult(verdict) => {
                match app.settings_view.as_mut() {
                    Some(s) => s.tool_test_status = Some(verdict),
                    None => notice!(&format!("Tool test: {verdict}")),
                }
                app.renderer.request_reprint();
            }
            UiEvent::ServerDiscovered(disc) => {
                let url = disc.base_url.to_lowercase();
                let is_lm_studio = url.contains("1234") || url.contains("lmstudio");
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

                        let ctx_tag = app.current_context.as_deref().unwrap_or("");
                        refresh_welcome_card_if_before_user_msg(
                            &mut app.chat,
                            &mut app.renderer,
                            &app.current_model,
                            &cwd_display,
                            perm.state().mode().label(),
                            memory_docs,
                            &source,
                            &app.current_effort,
                            app.current_context.as_deref(),
                            app.config.show_mascot,
                            mascot_mood,
                        );

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
                        // The model's own header when it wrote one, since it
                        // says what the call is for; the tool name otherwise.
                        let what = flashagent_llm::effective_args(args_json, name)
                            .and_then(|v| {
                                v.get("header").and_then(|h| h.as_str()).map(str::trim).filter(|h| !h.is_empty()).map(str::to_string)
                            })
                            .unwrap_or_else(|| format!("Running {name}"));
                        app.turn_phase = TurnPhase::Tool(what);
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
                    _ => {}
                }
                if let Some(ledger) = app.goal_ledger.as_mut() {
                    ledger.on_event(&e);
                    if matches!(e, LoopEvent::StepStarted { .. }) {
                        app.renderer.request_reprint();
                    }
                }
                app.chat.on_event(&e);
                if app.chat.take_needs_reprint() {
                    app.renderer.request_reprint();
                }
                update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
            }
            UiEvent::Finished { turn_id, result: res } => {
                if turn_id != app.turn_counter || app.aborted_turn == Some(turn_id) {
                    // A turn that was hard-aborted (or superseded) reporting late.
                    continue;
                }
                app.running = false;
                app.turn_started = None;
                app.active_turn_handle = None;
                app.active_steer_tx = None;
                app.cancel_requested = None;
                cancel.store(false, Ordering::Relaxed);
                app.token_tracker.on_finished();

                // What the turn cost against what it produced is the only
                // honest evidence about whether auto guessed right for this
                // model. A goal run is excluded: its effort is the user's.
                // What this turn cost the window is the best guess at what
                // the next one will cost.
                let grown = app.context_usage.total_used().saturating_sub(app.context_before_turn);
                if grown > 0 {
                    app.last_turn_growth = grown.max(app.last_turn_growth / 2);
                }
                if app.goal_state.is_none() {
                    let steps = app.effort_memory.observe(&app.current_model, &app.turn_outcome);
                    app.effort_memory.save();
                    source.set_effort_bias(steps);
                }
                app.turn_outcome = flashagent_core::TurnOutcome::default();

                // Roll back any temporary goal state
                if let Some(saved) = app.goal_state.take() {
                    tools_arc.set_goal_mode(false);
                    perm.state().set_mode(saved.mode);
                    app.current_effort = saved.effort.clone();
                    app.max_steps = saved.max_steps;
                    if let Some(ledger) = app.goal_ledger.take() {
                        let reason = match &res {
                            Ok((_, r)) => *r,
                            Err(_) => DoneReason::Failed,
                        };
                        push_goal_report(&mut app.chat, &ledger, reason);
                    }
                    notice!(&format!(
                        "[Goal \"{}\" finished · Restored mode to {} and thinking to {}]",
                        flashagent_tui::truncate_middle(&saved.task, 40),
                        saved.mode.label(),
                        saved.effort
                    ));
                }

                match res {
                    Ok((h, reason)) => {
                        app.history = h;
                        update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);

                        if reason == DoneReason::Cancelled {
                            close_dangling_user(&mut app.history, "[interrupted by the user before replying]");
                            app.suggested_prompt = None;
                            let interrupt_msg = "Request interrupted by user";
                            app.custom_placeholder = Some(interrupt_msg.to_string());
                            app.renderer.request_reprint();
                        } else {
                            // Summarise the older part of the conversation
                            // when the next turn would not fit, or the user's
                            // threshold is passed — and only when there is
                            // something worth summarising.
                            let verdict = if app.config.auto_compact_context && app.history.len() > 3 {
                                flashagent_core::should_compact(flashagent_core::CompactionInput {
                                    used: app.context_usage.total_used(),
                                    capacity: app.context_usage.total_capacity,
                                    fixed: app.context_usage.system_tokens
                                        + app.context_usage.memory_tokens
                                        + app.context_usage.tools_tokens,
                                    last_turn_growth: app.last_turn_growth,
                                    threshold_pct: flashagent_core::resolved_compact_threshold(
                                        app.config.context_compact_threshold,
                                        app.context_usage.total_capacity,
                                    ),
                                })
                            } else {
                                flashagent_core::CompactionVerdict::No
                            };
                            if verdict.should() {
                                // This one belongs in the transcript: it
                                // changes what the model remembers, which is
                                // the conversation itself.
                                app.chat.push_system("Compacting context...");
                                app.renderer.request_reprint();
                                let tg_speed = app.token_tracker.tg_3s();
                                app.renderer.frame(
                                    &app.chat,
                                    &gate,
                                    &question_gate,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    // The command that started this is gone from the
                                    // composer; its suggestion list must go with it.
                                    None,
                                    &app.context_usage,
                                    FrameState {
                                        input: &app.input,
                                        mode: perm.state().mode(),
                                        is_goal_active: app.goal_state.is_some(),
                                        goal_progress: None,
                                        tip: Some(app.tip_animator.tip_text),
                                        tip_animated: None,
                                        tip_lines: Some(&tip_lines),
                                        token_tracker: Some(&app.token_tracker),
                                        reasoning_expand: ReasoningExpansion {
                                            all: app.all_expanded,
                                            last: app.last_expanded,
                                        },
                                        tick_n: app.tick_n,
                                        running: app.running,
                                        elapsed_secs: app.turn_started.map(|t| t.elapsed().as_secs()).unwrap_or(0),
                                        face_phase: app.turn_started.map(|t| (t.elapsed().as_millis() / 80) as usize).unwrap_or(0),
                                        model_tokens: app.token_tracker.total_model_tokens,
                                        tokens_per_sec: tg_speed,
                                        f_keep: app.token_tracker.last_f_keep,
                                        confirm_selection: app.confirm_select.decision(),
                                        question_state: Some(&app.question_ui_state),
                                        custom_placeholder: app.custom_placeholder.as_deref(),
                                        suggested_prompt: app.suggested_prompt.as_deref(),
                                        copy_toast: None,
                                        prefill_status: None,
                                        ttft_display: None,
                                        background: app.background.as_ref().map(|b| b.text.as_str()),
                                        channel_prompt: None,
                                        quit_prompt: None,
                                        turn_phase: None,
                                        attachments: &[],
                background_style: app.background.as_ref().map_or(NoticeStyle::FULL, BackgroundNotice::style),
                                        context_warn_threshold: app.config.context_warn_threshold,
                                    },
                                );
                                let source_compact = source.clone();
                                let before = app.context_usage.total_used();
                                match compact_context(&source_compact, &mut app.history, None).await {
                                    Some(_) => {
                                        update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
                                        let saved = before.saturating_sub(app.context_usage.total_used());
                                        app.chat.replace_last_system(&format!(
                                            "Context compacted · {} saved · the conversation so far is now a summary",
                                            ContextUsage::format_tokens(saved)
                                        ));
                                    }
                                    None => app.chat.replace_last_system(
                                        "Compacting context failed — the conversation is unchanged",
                                    ),
                                }
                                app.renderer.request_reprint();
                            }

                            if app.config.auto_save_sessions {
                                save_session_file(&session_id, &app.current_model, &cwd_display, &app.history);
                            }

                            // Clear any existing ghost suggestion.
                            // No hardcoded or heuristic fallback strings for recap or write-in suggestions:
                            // they are ONLY shown if and when dynamically generated by the background LLM task.
                            app.suggested_prompt = None;
                            app.renderer.request_reprint();

                            // Asynchronously ask LLM in background for refined recap & contextual suggestion
                            let source_bg = source.clone();
                            let tx_bg = tx.clone();
                            let history_bg = app.history.clone();
                            let turn_id = app.chat.user_turn_count() as u64;
                            tokio::spawn(async move {
                                if let Some((llm_recap, llm_suggestion)) = generate_llm_recap_and_suggestion(&source_bg, &history_bg).await {
                                    let _ = tx_bg.send(UiEvent::BackgroundRecap {
                                        turn_id,
                                        recap: llm_recap,
                                        suggestion: llm_suggestion,
                                    });
                                }
                            });
                        }
                    }
                    Err((e, h)) => {
                        // Keep the steps that already ran (and changed files).
                        app.history = h;
                        close_dangling_user(&mut app.history, "[no reply: the model backend failed]");
                        update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
                        app.chat.on_event(&LoopEvent::Done(DoneReason::Failed));
                        // What broke and what to do about it, with the raw
                        // text kept underneath rather than as the headline.
                        let explained = flashagent_tui::backend_error::explain(
                            &e,
                            &app.config.backend_url,
                            &app.current_model,
                        );
                        app.chat.push_line(LineKind::ToolError, explained.headline.clone());
                        if let Some(hint) = &explained.hint {
                            // Its own line: an embedded newline is not wrapped
                            // by the renderer, it is clipped.
                            app.chat.push_line(
                                LineKind::System,
                                format!("  \x1b[38;2;160;155;145m{hint}\x1b[0m"),
                            );
                        }
                        if explained.headline != explained.raw.trim() {
                            app.chat.push_line(
                                LineKind::System,
                                format!("  \x1b[38;2;120;115;110m{}\x1b[0m", explained.raw.trim()),
                            );
                        }
                        notice!("Ctrl+R retries the last prompt");
                    }
                }
            }
            UiEvent::Resize(cols, rows) => {
                let term_resized = (cols, rows) != app.last_term_size;
                app.last_term_size = (cols, rows);
                if !app.chat.has_user_message() && term_resized {
                    refresh_welcome_card_animated(
                        &mut app.chat,
                        &mut app.renderer,
                        &app.current_model,
                        &cwd_display,
                        perm.state().mode().label(),
                        memory_docs,
                        &source,
                        &app.current_effort,
                        app.current_context.as_deref(),
                        app.tick_n,
                        Some(cols as usize),
                        app.config.show_mascot,
                        mascot_mood,
                        None,
                    );
                }
                app.renderer.request_reprint();
            }
            UiEvent::Mouse(m) => {
                let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                let total_chat_lines = app.chat.render_split(width as usize, ReasoningExpansion { all: app.all_expanded, last: app.last_expanded }).0.len() + 15;
                let max_scroll = total_chat_lines.saturating_sub(height as usize);
                if let Some(ref mut menu) = app.model_menu {
                    match m.kind {
                        MouseEventKind::ScrollUp => menu.up(),
                        MouseEventKind::ScrollDown => menu.down(),
                        _ => {}
                    }
                    app.renderer.request_reprint();
                } else if let Some(ref mut menu) = app.effort_menu {
                    match m.kind {
                        MouseEventKind::ScrollUp => menu.up(),
                        MouseEventKind::ScrollDown => menu.down(),
                        _ => {}
                    }
                    app.renderer.request_reprint();
                } else {
                    match m.kind {
                        MouseEventKind::ScrollUp => {
                            app.renderer.scroll_up(3, max_scroll);
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
                    } else if let Some(ref mut sm) = app.sampling_view {
                        for ch in sanitized.chars() {
                            sm.handle_key(KeyCode::Char(ch), KeyModifiers::NONE);
                        }
                    } else if app.effort_menu.is_none() && app.model_menu.is_none() && app.settings_view.is_none() && app.context_modal.is_none() && app.mcp_modal.is_none() {
                        app.input.push_str(&sanitized);
                        app.history_index = None;
                        app.autocomplete_idx = 0;
                    }
                    app.renderer.request_reprint();
                }
            }
            UiEvent::Key(code, mods) => {
                // The channel card owns the keyboard while it is up: it is a
                // yes-or-no about replacing the binary, and typing past it
                // would leave the answer ambiguous.
                if app.quit_confirm {
                    app.quit_confirm = false;
                    let yes = matches!(
                        code,
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('\u{043d}')
                            | KeyCode::Char('\u{041d}') | KeyCode::Enter
                    );
                    if yes {
                        break 'main_loop;
                    }
                    app.renderer.request_reprint();
                    continue;
                }

                if let Some(sw) = app.channel_switch.take() {
                    let yes = matches!(
                        code,
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('\u{043d}')
                            | KeyCode::Char('\u{041d}') | KeyCode::Enter
                    );
                    if yes {
                        app.config.update_channel = sw.to;
                        let _ = app.config.save();
                        if channel_watch_tx.receiver_count() > 0 {
                            let _ = channel_watch_tx.send(sw.to);
                        }
                        if let Some(ref mut view) = app.settings_view {
                            view.config.update_channel = sw.to;
                        }
                        app.background = Some(BackgroundNotice::sticky(format!(
                            "Release channel is now {} \u{b7} press Ctrl+U to move to it",
                            sw.to.label()
                        )));
                    } else {
                        // Everything else the user changed stayed applied; only
                        // this one is put back.
                        if let Some(ref mut view) = app.settings_view {
                            view.config.update_channel = sw.from;
                        }
                        app.background = Some(BackgroundNotice::fading(
                            format!("Still on the {} channel", sw.from.label()),
                            5,
                        ));
                    }
                    app.renderer.request_reprint();
                    continue;
                }
                // If settings view is open, it captures all keyboard input
                if let Some(ref mut settings) = app.settings_view {
                    // What the view was opened with (live session values).
                    let shown_mode = app.goal_state.as_ref().map_or(perm.state().mode(), |g| g.mode);
                    let shown_effort = app.goal_state.as_ref().map_or(app.current_effort.clone(), |g| g.effort.clone());
                    let action = settings.handle_key(code, mods);
                    match action {
                        SettingsAction::Close => {
                            let old_model = app.current_model.clone();
                            let old_effort = shown_effort.clone();
                            let old_mode = shown_mode;
                            let chosen_mode = settings.config.permission_mode;
                            let chosen_effort = settings.config.thinking_effort.clone();
                            let mut changes = Vec::new();
                            if settings.config.permission_mode != old_mode {
                                changes.push(format!("mode ({})", settings.config.permission_mode.label()));
                            }
                            if settings.config.model != old_model {
                                changes.push(format!("model ({})", settings.config.model));
                            }
                            if settings.config.thinking_effort != old_effort {
                                changes.push(format!("thinking ({})", settings.config.thinking_effort));
                            }

                            if changes.len() == 1 && settings.config.permission_mode != old_mode {
                                app.custom_placeholder = Some(format!("Permission mode set to: {}", settings.config.permission_mode.label()));
                            } else if !changes.is_empty() {
                                app.custom_placeholder = Some(format!("Settings updated: {}", changes.join(", ")));
                            }
                            app.suggested_prompt = None;

                            // Every other setting is applied now; the release
                            // channel waits for an answer, so declining costs
                            // the user nothing else they just changed.
                            let wanted_channel = settings.config.update_channel;
                            let mut applied = persisted_from_view(&settings.config, &app.config, shown_mode, &shown_effort);
                            if wanted_channel != app.config.update_channel {
                                applied.update_channel = app.config.update_channel;
                                app.custom_placeholder = None;
                                app.channel_switch = Some(ChannelSwitch {
                                    from: app.config.update_channel,
                                    to: wanted_channel,
                                    target: ChannelTarget::Checking,
                                });
                                let tx_ch = channel_probe_tx.clone();
                                tokio::spawn(async move {
                                    let target = match flashagent_svc::updater::newest_on_channel(
                                        wanted_channel,
                                        flashagent_svc::updater::DEFAULT_RELEASES_API,
                                    )
                                    .await
                                    {
                                        Ok(Some(version)) => ChannelTarget::Version(version),
                                        Ok(None) => ChannelTarget::Empty,
                                        Err(_) => ChannelTarget::Unknown,
                                    };
                                    let _ = tx_ch.send(target);
                                });
                            }
                            app.config = applied;
                            let _ = app.config.save();
                            tools_arc.set_toolset_profile(app.config.toolset_profile);
                            tools_arc.set_web_enabled(app.config.free_search);
                            source.0.set_max_retries(app.config.network_retries);
                            if app.config.model != app.current_model {
                                app.current_model = app.config.model.clone();
                                source.set_model(&app.current_model);
                                source.set_effort_bias(app.effort_memory.steps(&app.current_model));
                                tools_arc.set_vision_supported(model_sees_images(&source, &app.current_model));
                            }
                            // During /goal the live mode/effort are the goal's;
                            // edits apply to what the goal restores afterwards.
                            match app.goal_state.as_mut() {
                                Some(g) => {
                                    g.mode = chosen_mode;
                                    g.effort = chosen_effort;
                                }
                                None => {
                                    perm.state().set_mode(chosen_mode);
                                    app.current_effort = chosen_effort;
                                }
                            }
                            app.settings_view = None;
                            refresh_welcome_card_if_before_user_msg(
                                &mut app.chat,
                                &mut app.renderer,
                                &app.current_model,
                                &cwd_display,
                                perm.state().mode().label(),
                                memory_docs,
                                &source,
                                &app.current_effort,
                                app.current_context.as_deref(),
                                app.config.show_mascot,
                                mascot_mood,
                            );
                            app.renderer.request_reprint();
                        }
                        SettingsAction::DiscoverModels => {
                            app.config = persisted_from_view(&settings.config, &app.config, shown_mode, &shown_effort);
                            let _ = app.config.save();
                            // Probe the URL the user just typed, not the one
                            // this session is connected to.
                            let key = settings.config.api_key.clone().or_else(|| std::env::var("FLASHAGENT_API_KEY").ok());
                            let probe = flashagent_llm::OpenAiCompat::new(&settings.config.backend_url, "", key);
                            settings.available_models = match probe.discover_server().await {
                                Some(disc) => disc.models.into_iter().map(|m| m.id).collect(),
                                None => Vec::new(),
                            };
                            if settings.config.backend_url.trim_end_matches('/') != source.0.base_url() {
                                app.custom_placeholder = Some("Backend URL saved; restart FlashAgent to connect to it".to_string());
                            }
                            app.renderer.request_reprint();
                        }
                        SettingsAction::RunToolTest => {
                            settings.tool_test_status = Some(format!("Probing {}...", app.current_model));
                            let source_bg = source.clone();
                            let tx_bg = tx.clone();
                            tokio::spawn(async move {
                                let verdict = run_tool_call_probe(&source_bg).await;
                                let _ = tx_bg.send(UiEvent::ToolTestResult(verdict));
                            });
                            app.renderer.request_reprint();
                        }
                        SettingsAction::OpenModelMenu => {
                            app.config = persisted_from_view(&settings.config, &app.config, shown_mode, &shown_effort);
                            let _ = app.config.save();
                            app.settings_view = None;
                            if let Some(mut menu) = build_model_menu(&source) {
                                menu.select_by_value(&app.current_model);
                                app.model_menu = Some(menu);
                            } else {
                                notice!("[No models discovered from server]");
                            }
                            app.renderer.request_reprint();
                        }
                        SettingsAction::OpenEffortMenu => {
                            app.config = persisted_from_view(&settings.config, &app.config, shown_mode, &shown_effort);
                            let _ = app.config.save();
                            app.settings_view = None;
                            let mut menu = build_effort_menu(&source, &app.effort_memory, &app.current_model);
                            menu.select_by_value(&app.current_effort);
                            app.effort_menu = Some(menu);
                            app.renderer.request_reprint();
                        }
                        SettingsAction::OpenWizard => {
                            app.config = persisted_from_view(&settings.config, &app.config, shown_mode, &shown_effort);
                            let _ = app.config.save();
                            app.settings_view = None;
                            let completed = flashagent_tui::run_wizard_channel(&mut app.config, &mut rx).await.unwrap_or(false);
                            if completed {
                                app.current_model = app.config.model.clone();
                                source.set_model(&app.current_model);
                                source.set_effort_bias(app.effort_memory.steps(&app.current_model));
                                tools_arc.set_vision_supported(model_sees_images(&source, &app.current_model));
                                app.current_effort = app.config.thinking_effort.clone();
                                perm.state().set_mode(app.config.permission_mode);
                                refresh_welcome_card_if_before_user_msg(
                                    &mut app.chat,
                                    &mut app.renderer,
                                    &app.current_model,
                                    &cwd_display,
                                    perm.state().mode().label(),
                                    memory_docs,
                                    &source,
                                    &app.current_effort,
                                    app.current_context.as_deref(),
                                    app.config.show_mascot,
                                    mascot_mood,
                                );
                            }
                            app.renderer.request_reprint();
                        }
                        SettingsAction::OpenSamplingMenu => {
                            app.config = persisted_from_view(&settings.config, &app.config, shown_mode, &shown_effort);
                            let _ = app.config.save();
                            app.settings_view = None;
                            app.sampling_view = Some(SamplingView::new(&app.config));
                            app.renderer.request_reprint();
                        }
                        SettingsAction::CheckUpdatesNow => {
                            if flashagent_svc::updater::is_dev_mode() {
                                settings.update_check_status = Some("Dev mode: updates disabled (source build)".into());
                            } else {
                                settings.update_check_status = Some("Checking GitHub releases...".into());
                                let ch = settings.config.update_channel;
                                let update_tx_clone = update_tx.clone();
                                tokio::spawn(async move {
                                    match flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                                        Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                            let _ = update_tx_clone.send(UpdateNotice::Available { version: target, asset_name, download_url, checksums_url });
                                        }
                                        Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => {
                                            let _ = update_tx_clone.send(UpdateNotice::UpToDate { version: current });
                                        }
                                        Err(e) => {
                                            let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                                        }
                                    }
                                });
                            }
                            app.renderer.request_reprint();
                        }
                        SettingsAction::OpenMcpMenu => {
                            app.config = persisted_from_view(&settings.config, &app.config, shown_mode, &shown_effort);
                            let _ = app.config.save();
                            app.settings_view = None;
                            let mgr = tools_arc.mcp_manager();
                            let paths = mgr.loaded_paths();
                            let statuses = mgr.server_status_list().await;
                            app.mcp_modal = Some(McpModal::new(paths, statuses, McpViewTab::Overview));
                            app.renderer.request_reprint();
                        }
                        SettingsAction::None => {
                            app.renderer.request_reprint();
                        }
                    }
                    continue;
                }

                // If sampling parameters view is open, it captures all keyboard input
                if let Some(ref mut sm) = app.sampling_view {
                    let action = sm.handle_key(code, mods);
                    match action {
                        SamplingAction::Close => {
                            app.sampling_view = None;
                            app.renderer.request_reprint();
                        }
                        SamplingAction::SaveAndClose => {
                            sm.apply_to_config(&mut app.config);
                            let _ = app.config.save();
                            app.sampling_view = None;
                            app.custom_placeholder = Some("Sampling parameters updated".to_string());
                            app.suggested_prompt = None;
                            app.renderer.request_reprint();
                        }
                        SamplingAction::None => {
                            app.renderer.request_reprint();
                        }
                    }
                    continue;
                }

                // The memory screen owns every key while it is up: 'd' and
                // 'e' are commands on the list and letters inside a note.
                if let Some(ref mut modal) = app.memory_modal {
                    use flashagent_tui::memory_view::MemoryAction;
                    match modal.handle_key(code, mods) {
                        MemoryAction::None => {}
                        MemoryAction::Close => {
                            app.memory_modal = None;
                        }
                        MemoryAction::Forget { name, scope } => {
                            let cwd = std::env::current_dir().unwrap_or_default();
                            let removed = flashagent_core::MemoryStore::for_scope(scope, &cwd)
                                .map(|store| store.remove(&name).unwrap_or(false))
                                .unwrap_or(false);
                            notice!(&if removed {
                                format!("[Forgot \"{name}\"]")
                            } else {
                                format!("[Could not forget \"{name}\"]")
                            });
                            app.memory_modal = Some(flashagent_tui::memory_view::MemoryModal::new(&cwd));
                        }
                        MemoryAction::Tell { message } => {
                            // Sent through the ordinary path, so it is an
                            // ordinary turn: the model decides what to change
                            // and says so in the chat.
                            app.memory_modal = None;
                            app.input = message;
                            let _ = tx.send(UiEvent::Key(KeyCode::Enter, KeyModifiers::NONE));
                        }
                    }
                    app.renderer.request_reprint();
                    continue;
                }

                // If context modal is open, F1, Enter, Esc or 'q' closes it
                if app.context_modal.is_some() {
                    if matches!(code, KeyCode::F(1) | KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q')) {
                        app.context_modal = None;
                        app.renderer.request_reprint();
                    }
                    continue;
                }

                // If MCP modal is open, it captures navigation and actions
                if let Some(ref mut modal) = app.mcp_modal {
                    match modal.handle_key(code, mods) {
                        McpModalAction::Close => {
                            app.mcp_modal = None;
                            app.renderer.request_reprint();
                        }
                        McpModalAction::Reload => {
                            let mgr = tools_arc.mcp_manager();
                            let _ = mgr.reload().await;
                            modal.servers = mgr.server_status_list().await;
                            modal.status_message = Some("Reloaded MCP configurations".to_string());
                            app.renderer.request_reprint();
                        }
                        McpModalAction::TestServer(name) => {
                            modal.status_message = Some(format!("Testing {name}..."));
                            app.renderer.request_reprint();
                            let mgr = tools_arc.mcp_manager();
                            match mgr.test_server(&name).await {
                                Ok(report) => {
                                    let ver = report.server_version.as_deref().unwrap_or("1.0.0");
                                    let lat = format!("{:.1}ms", report.latency.as_secs_f64() * 1000.0);
                                    modal.status_message = Some(format!("✔ {name} connected (v{ver}, {lat}, {} tools)", report.tools.len()));
                                }
                                Err(e) => {
                                    modal.status_message = Some(format!("✕ {name} failed: {e}"));
                                }
                            }
                            modal.servers = mgr.server_status_list().await;
                            app.renderer.request_reprint();
                        }
                        McpModalAction::InstallMarketplace(id) => {
                            if let Some(item) = flashagent_tools::mcp::find_marketplace_item(&id) {
                                let cfg = flashagent_tools::mcp::scaffold_config(item);
                                match flashagent_tools::mcp::save_server_to_project(std::path::Path::new("."), item.id, cfg) {
                                    Ok(path) => {
                                        modal.status_message = Some(format!("✔ Added {} to {}", item.name, path.display()));
                                        let mgr = tools_arc.mcp_manager();
                                        let server_id = item.id.to_string();
                                        let mgr_clone = mgr.clone();
                                        tokio::spawn(async move {
                                            let _ = mgr_clone.reload().await;
                                            let _ = mgr_clone.start_server(&server_id).await;
                                        });
                                    }
                                    Err(e) => {
                                        modal.status_message = Some(format!("✕ Failed to install {id}: {e}"));
                                    }
                                }
                            }
                            app.renderer.request_reprint();
                        }
                        McpModalAction::None => {
                            app.renderer.request_reprint();
                        }
                    }
                    continue;
                }

                // If model selection menu is open, it captures navigation
                if let Some(ref mut menu) = app.model_menu {
                    match code {
                        KeyCode::Up => menu.up(),
                        KeyCode::Down => menu.down(),
                        KeyCode::PageUp => menu.page_up(),
                        KeyCode::PageDown => menu.page_down(),
                        KeyCode::Backspace => menu.pop_filter_char(),
                        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                            menu.push_filter_char(c);
                        }
                        KeyCode::Enter => {
                            if let Some(val) = menu.selected_value() {
                                app.current_model = val.clone();
                                app.config.model = app.current_model.clone();
                                let _ = app.config.save();
                                source.set_model(&app.current_model);
                                source.set_effort_bias(app.effort_memory.steps(&app.current_model));
                                tools_arc.set_vision_supported(model_sees_images(&source, &app.current_model));
                                if let Some(disc) = source.discovery() {
                                    if let Some(m) = disc.models.iter().find(|m| m.id == app.current_model) {
                                        app.current_context = m.context_display();
                                        // The effort the user picked survives
                                        // the switch; a model that cannot
                                        // reason just receives no thinking
                                        // fields.
                                        if app.current_effort.is_empty() {
                                            app.current_effort = "auto".to_string();
                                        }
                                    }
                                }
                                refresh_welcome_card_if_before_user_msg(
                                    &mut app.chat,
                                    &mut app.renderer,
                                    &app.current_model,
                                    &cwd_display,
                                    perm.state().mode().label(),
                                    memory_docs,
                                    &source,
                                    &app.current_effort,
                                    app.current_context.as_deref(),
                                    app.config.show_mascot,
                                    mascot_mood,
                                );
                                app.custom_placeholder = Some(format!("Switched active model to: {}", app.current_model));
                                app.suggested_prompt = None;
                                app.renderer.request_reprint();
                            }
                            app.model_menu = None;
                        }
                        KeyCode::Esc => {
                            app.model_menu = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                // If effort selection menu is open, it captures navigation
                if let Some(ref mut menu) = app.effort_menu {
                    match code {
                        KeyCode::Up => menu.up(),
                        KeyCode::Down => menu.down(),
                        KeyCode::Enter => {
                            if let Some(val) = menu.selected_value() {
                                app.current_effort = val.clone();
                                refresh_welcome_card_if_before_user_msg(
                                    &mut app.chat,
                                    &mut app.renderer,
                                    &app.current_model,
                                    &cwd_display,
                                    perm.state().mode().label(),
                                    memory_docs,
                                    &source,
                                    &app.current_effort,
                                    app.current_context.as_deref(),
                                    app.config.show_mascot,
                                    mascot_mood,
                                );
                                app.custom_placeholder = Some(format!("Thinking effort set to: {}", app.current_effort));
                                app.suggested_prompt = None;
                                app.renderer.request_reprint();
                            }
                            app.effort_menu = None;
                        }
                        KeyCode::Esc => {
                            app.effort_menu = None;
                        }
                        _ => {}
                    }
                    continue;
                }

                if let Some(req) = question_gate.pending() {
                    match code {
                        // Ctrl+C interrupts the turn, as everywhere else while it runs.
                        KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                            if mods.contains(KeyModifiers::CONTROL) =>
                        {
                            app.question_ui_state = QuestionUiState::default();
                            question_gate.cancel();
                            if app.running && app.cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                app.cancel_requested = Some(std::time::Instant::now());
                                app.turn_phase = TurnPhase::Stopping;
                                app.active_steer_tx = None;
                                app.custom_placeholder = Some("Interrupting...".to_string());
                            }
                        }
                        KeyCode::Esc => {
                            if req.options.is_some() && app.question_ui_state.is_writing {
                                app.question_ui_state.is_writing = false;
                            } else {
                                app.question_ui_state = QuestionUiState::default();
                                question_gate.cancel();
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(ref opts) = req.options {
                                let total_choices = opts.len();
                                if app.question_ui_state.is_writing {
                                    let answer = std::mem::take(&mut app.question_ui_state.write_in_text);
                                    let trimmed = answer.trim().to_string();
                                    if !trimmed.is_empty() {
                                        if req.multi_select && !app.question_ui_state.selected_indices.is_empty() {
                                            let mut chosen: Vec<String> = app.question_ui_state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect();
                                            chosen.push(trimmed);
                                            app.question_ui_state = QuestionUiState::default();
                                            question_gate.respond(chosen.join(", "), true);
                                        } else {
                                            app.question_ui_state = QuestionUiState::default();
                                            question_gate.respond(trimmed, true);
                                        }
                                    }
                                } else if app.question_ui_state.selected_index == total_choices {
                                    app.question_ui_state.is_writing = true;
                                    app.question_ui_state.write_in_text.clear();
                                } else if req.multi_select {
                                    let mut chosen: Vec<String> = app.question_ui_state.selected_indices.iter().filter_map(|&i| opts.get(i).cloned()).collect();
                                    if chosen.is_empty() && app.question_ui_state.selected_index < total_choices {
                                        chosen.push(opts[app.question_ui_state.selected_index].clone());
                                    }
                                    app.question_ui_state = QuestionUiState::default();
                                    question_gate.respond(chosen.join(", "), false);
                                } else if app.question_ui_state.selected_index < total_choices {
                                    let chosen = opts[app.question_ui_state.selected_index].clone();
                                    app.question_ui_state = QuestionUiState::default();
                                    question_gate.respond(chosen, false);
                                }
                            } else {
                                let answer = std::mem::take(&mut app.question_ui_state.write_in_text);
                                let trimmed = answer.trim().to_string();
                                app.question_ui_state = QuestionUiState::default();
                                question_gate.respond(trimmed, true);
                            }
                        }
                        KeyCode::Up => {
                            if !app.question_ui_state.is_writing {
                                if let Some(ref opts) = req.options {
                                    let total = opts.len() + 1;
                                    if app.question_ui_state.selected_index == 0 {
                                        app.question_ui_state.selected_index = total.saturating_sub(1);
                                    } else {
                                        app.question_ui_state.selected_index -= 1;
                                    }
                                }
                            }
                        }
                        KeyCode::Down => {
                            if !app.question_ui_state.is_writing {
                                if let Some(ref opts) = req.options {
                                    let total = opts.len() + 1;
                                    app.question_ui_state.selected_index = (app.question_ui_state.selected_index + 1) % total;
                                }
                            }
                        }
                        KeyCode::Backspace => {
                            if app.question_ui_state.is_writing || req.options.is_none() {
                                app.question_ui_state.write_in_text.pop();
                            }
                        }
                        KeyCode::Char(' ') if req.multi_select && !app.question_ui_state.is_writing => {
                            if let Some(ref opts) = req.options {
                                let total_choices = opts.len();
                                if app.question_ui_state.selected_index < total_choices {
                                    let idx = app.question_ui_state.selected_index;
                                    if app.question_ui_state.selected_indices.contains(&idx) {
                                        app.question_ui_state.selected_indices.remove(&idx);
                                    } else {
                                        app.question_ui_state.selected_indices.insert(idx);
                                    }
                                } else {
                                    app.question_ui_state.is_writing = true;
                                    app.question_ui_state.write_in_text.clear();
                                }
                            }
                        }
                        KeyCode::Char(c) if !mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT) => {
                            if app.question_ui_state.is_writing || req.options.is_none() {
                                app.question_ui_state.write_in_text.push(c);
                            } else if let Some(ref opts) = req.options {
                                let total = opts.len() + 1;
                                if let Some(d) = c.to_digit(10) {
                                    let idx = (d as usize).saturating_sub(1);
                                    if idx < opts.len() {
                                        if req.multi_select {
                                            if app.question_ui_state.selected_indices.contains(&idx) {
                                                app.question_ui_state.selected_indices.remove(&idx);
                                            } else {
                                                app.question_ui_state.selected_indices.insert(idx);
                                            }
                                        }
                                        app.question_ui_state.selected_index = idx;
                                    } else if idx == total - 1 {
                                        app.question_ui_state.selected_index = idx;
                                        app.question_ui_state.is_writing = true;
                                        app.question_ui_state.write_in_text.clear();
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    app.renderer.request_reprint();
                    continue;
                }

                if gate.pending().is_some() {
                    match code {
                        // Ctrl+C refuses the call and interrupts the turn.
                        KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                            if mods.contains(KeyModifiers::CONTROL) =>
                        {
                            gate.respond(Decision::Deny);
                            app.confirm_select = ConfirmSelect::new();
                            if app.running && app.cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                app.cancel_requested = Some(std::time::Instant::now());
                                app.turn_phase = TurnPhase::Stopping;
                                app.active_steer_tx = None;
                                app.custom_placeholder = Some("Interrupting...".to_string());
                            }
                        }
                        KeyCode::Esc
                        | KeyCode::Char('d') | KeyCode::Char('D') | KeyCode::Char('\u{0432}') | KeyCode::Char('\u{0412}') => {
                            gate.respond(Decision::Deny);
                            app.confirm_select = ConfirmSelect::new();
                        }
                        KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('\u{0444}') | KeyCode::Char('\u{0424}') => {
                            if let Some(req) = gate.pending() {
                                // Narrow rules for shell (`npm test` never
                                // covers `npm publish`); tool-wide otherwise.
                                let rules = perm.state().allow_always(&req);
                                if rules.is_empty() {
                                    notice!("[Allowed once: this command cannot be saved as a narrow rule]");
                                } else {
                                    notice!(&format!("[Always allowed this session: {}]", rules.join(", ")));
                                }
                            }
                            gate.respond(Decision::Allow);
                            app.confirm_select = ConfirmSelect::new();
                        }
                        KeyCode::Enter => {
                            gate.respond(app.confirm_select.decision());
                            app.confirm_select = ConfirmSelect::new();
                        }
                        KeyCode::Left => {
                            app.confirm_select.left();
                        }
                        KeyCode::Right => {
                            app.confirm_select.right();
                        }
                        KeyCode::Tab | KeyCode::Up | KeyCode::Down => {
                            app.confirm_select.toggle();
                        }
                        _ => {}
                    }
                    app.renderer.request_reprint();
                    continue;
                }

                // --- Chat History Scrolling & Navigation ---
                if matches!(code, KeyCode::PageUp) {
                    let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    let total_chat_lines = app.chat.render_split(width as usize, ReasoningExpansion { all: app.all_expanded, last: app.last_expanded }).0.len() + 15;
                    let max_scroll = total_chat_lines.saturating_sub(height as usize);
                    app.renderer.scroll_up((height as usize / 2).max(5), max_scroll);
                    continue;
                }
                if matches!(code, KeyCode::PageDown) {
                    let (_, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    app.renderer.scroll_down((height as usize / 2).max(5));
                    continue;
                }
                if matches!(code, KeyCode::Home) && app.renderer.scroll_offset > 0 {
                    let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    let total_chat_lines = app.chat.render_split(width as usize, ReasoningExpansion { all: app.all_expanded, last: app.last_expanded }).0.len() + 15;
                    let max_scroll = total_chat_lines.saturating_sub(height as usize);
                    app.renderer.scroll_up(max_scroll, max_scroll);
                    continue;
                }
                if matches!(code, KeyCode::End) && app.renderer.scroll_offset > 0 {
                    app.renderer.scroll_to_bottom();
                    continue;
                }
                if matches!(code, KeyCode::Esc) && app.renderer.scroll_offset > 0 {
                    app.renderer.scroll_to_bottom();
                    continue;
                }
                // Shift+Up, Ctrl+Up, Alt+Up -> scroll chat up.
                // If already scrolled up (scroll_offset > 0), plain Up also scrolls chat up!
                if (matches!(code, KeyCode::Up) && (mods.contains(KeyModifiers::SHIFT) || mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)))
                    || (app.renderer.scroll_offset > 0 && matches!(code, KeyCode::Up))
                {
                    let (width, height) = crossterm::terminal::size().unwrap_or((100, 24));
                    let total_chat_lines = app.chat.render_split(width as usize, ReasoningExpansion { all: app.all_expanded, last: app.last_expanded }).0.len() + 15;
                    let max_scroll = total_chat_lines.saturating_sub(height as usize);
                    app.renderer.scroll_up(2, max_scroll);
                    continue;
                }
                // Shift+Down, Ctrl+Down, Alt+Down -> scroll chat down.
                // If already scrolled up (scroll_offset > 0), plain Down also scrolls chat down!
                if (matches!(code, KeyCode::Down) && (mods.contains(KeyModifiers::SHIFT) || mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT)))
                    || (app.renderer.scroll_offset > 0 && matches!(code, KeyCode::Down))
                {
                    app.renderer.scroll_down(2);
                    continue;
                }

                if app.renderer.scroll_offset > 0 && matches!(code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Enter) {
                    app.renderer.scroll_to_bottom();
                }

                match code {
                    KeyCode::Esc => {
                        if app.running {
                            // Cooperative: the loop answers pending tool calls,
                            // keeps partial text and returns its history via
                            // Finished, so model memory matches the screen.
                            if app.cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                app.cancel_requested = Some(std::time::Instant::now());
                                app.turn_phase = TurnPhase::Stopping;
                                app.active_steer_tx = None;
                                app.suggested_prompt = None;
                                app.custom_placeholder = Some("Interrupting...".to_string());
                            }
                            app.renderer.request_reprint();
                        } else if !app.input.is_empty() {
                            app.input.clear();
                            app.autocomplete_idx = 0;
                            app.history_index = None;
                            if app.latest_suggestion.is_some() {
                                app.suggested_prompt = app.latest_suggestion.clone();
                            }
                            app.renderer.request_reprint();
                        } else if app.mcp_modal.is_some() {
                            app.mcp_modal = None;
                            app.renderer.request_reprint();
                        } else if app.suggested_prompt.is_some() || app.custom_placeholder.is_some() {
                            app.suggested_prompt = None;
                            app.latest_suggestion = None;
                            app.custom_placeholder = None;
                            app.renderer.request_reprint();
                        } else {
                            app.quit_confirm = true;
                            app.renderer.request_reprint();
                        }
                    }
                    // F1: toggle context modal
                    KeyCode::F(1) => {
                        if app.context_modal.is_some() {
                            app.context_modal = None;
                        } else {
                            app.effort_menu = None;
                            app.model_menu = None;
                            app.settings_view = None;
                            app.sampling_view = None;
                            app.mcp_modal = None;
                            app.context_modal = Some(ContextModal::new(app.context_usage.clone()));
                        }
                        app.renderer.request_reprint();
                    }

                    // Ctrl+C / Ctrl+Shift+C (handles both Latin and alternate physical keycodes)
                    KeyCode::Char('c') | KeyCode::Char('C') | KeyCode::Char('\u{0441}') | KeyCode::Char('\u{0421}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        if app.running {
                            // Cooperative: the loop answers pending tool calls,
                            // keeps partial text and returns its history via
                            // Finished, so model memory matches the screen.
                            if app.cancel_requested.is_none() {
                                cancel.store(true, Ordering::Relaxed);
                                app.cancel_requested = Some(std::time::Instant::now());
                                app.turn_phase = TurnPhase::Stopping;
                                app.active_steer_tx = None;
                                app.suggested_prompt = None;
                                app.custom_placeholder = Some("Interrupting...".to_string());
                            }
                            app.renderer.request_reprint();
                        } else {
                            let now = std::time::Instant::now();
                            let is_double_tap = app.last_ctrl_c.map(|t| now.duration_since(t).as_millis() < 1200).unwrap_or(false);
                            app.last_ctrl_c = Some(now);

                            if !app.input.is_empty() {
                                flashagent_tui::clipboard::set_clipboard_text(&app.input);
                                app.copy_toast = Some(("Copied input to clipboard".to_string(), now));
                                app.renderer.request_reprint();
                            } else if let Some(text) = app.chat.last_assistant_text() {
                                if is_double_tap {
                                    break 'main_loop;
                                }
                                flashagent_tui::clipboard::set_clipboard_text(&text);
                                app.copy_toast = Some(("Copied assistant response (press Ctrl+C again to exit)".to_string(), now));
                                app.renderer.request_reprint();
                            } else {
                                break 'main_loop;
                            }
                        }
                    }

                    // Ctrl+V / Ctrl+Shift+V: paste from clipboard (supports alternative keyboard layouts)
                    KeyCode::Char('v') | KeyCode::Char('V') | KeyCode::Char('\u{043c}') | KeyCode::Char('\u{041c}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        // A screenshot on the clipboard is what the user
                        // means by paste far more often than the file path
                        // of one, so it is looked for first.
                        if let Some(image) = flashagent_tui::clipboard::get_clipboard_image() {
                            maybe_measure_image_cost(
                                &app.image_costs,
                                &mut app.image_cost_probe,
                                &app.current_model,
                                &source,
                                &tx,
                            );
                            let att = Attachment::from_clipboard(image);
                            let label = att.label();
                            app.attachments.push(att);
                            if !model_sees_images(&source, &app.current_model) {
                                app.background = Some(BackgroundNotice::sticky(format!(
                                    "{label} attached · {} cannot see images — press F3 for one that can", app.current_model
                                )));
                            } else {
                                app.background = Some(BackgroundNotice::fading(
                                    format!("{label} attached · Ctrl+Z removes it"),
                                    8,
                                ));
                            }
                            app.renderer.request_reprint();
                        } else if let Some(text) = flashagent_tui::clipboard::get_clipboard_text() {
                            let sanitized = text.replace("\r\n", " ").replace(['\n', '\r'], " ");
                            if !sanitized.is_empty() {
                                app.input.push_str(&sanitized);
                                app.history_index = None;
                                app.autocomplete_idx = 0;
                                app.renderer.request_reprint();
                            }
                        }
                    }

                    // Ctrl+Z: take back the last picture attached.
                    KeyCode::Char('z') | KeyCode::Char('Z') | KeyCode::Char('\u{044f}') | KeyCode::Char('\u{042f}')
                        if mods.contains(KeyModifiers::CONTROL) && !app.attachments.is_empty() =>
                    {
                        if let Some(removed) = app.attachments.pop() {
                            app.background = Some(BackgroundNotice::fading(
                                format!("{} removed", removed.label()),
                                5,
                            ));
                        }
                        app.renderer.request_reprint();
                    }

                    // Ctrl+D: exit on empty input when idle
                    KeyCode::Char('d') | KeyCode::Char('D')
                        if mods.contains(KeyModifiers::CONTROL) && app.input.is_empty() && !app.running =>
                    {
                        break 'main_loop;
                    }

                    // Ctrl+R / Ctrl+Shift+R: regenerate last response from scratch (supports alternative keyboard layouts)
                    KeyCode::Char('r') | KeyCode::Char('R') | KeyCode::Char('\u{043a}') | KeyCode::Char('\u{041a}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        if !app.running
                            && gate.pending().is_none()
                            && question_gate.pending().is_none()
                            && app.effort_menu.is_none()
                            && app.model_menu.is_none()
                            && app.settings_view.is_none()
                            && app.sampling_view.is_none()
                            && app.context_modal.is_none()
                        {
                            if let Some(user_idx) = app.history.iter().rposition(|m| m.role == flashagent_llm::Role::User) {
                                // Asking for the same answer again is the
                                // user saying the last one was not good
                                // enough — the one signal that the model was
                                // given too little room to think.
                                let steps = app.effort_memory.observe(
                                    &app.current_model,
                                    &flashagent_core::TurnOutcome { regenerated: true, ..Default::default() },
                                );
                                app.effort_memory.save();
                                source.set_effort_bias(steps);
                                app.history.truncate(user_idx + 1);
                                app.chat.truncate_to_last_user();
                                app.renderer.scroll_to_bottom();
                                app.renderer.printed_settled = 0;
                                app.renderer.prev_expansion = None;
                                app.renderer.request_reprint();
                                update_context_usage(&mut app.context_usage, &app.history, &memory_block, &app.chat, perm);
                                cancel.store(false, Ordering::Relaxed);
                                app.suggested_prompt = None;
                                app.custom_placeholder = None;
                                app.last_expanded = false;
                                app.running = true;
                                app.turn_phase = TurnPhase::Waiting;
                                app.turn_started = Some(std::time::Instant::now());
                                app.token_tracker.on_turn_start(app.current_model.clone(), app.context_usage.total_used());
                                source.set_model(&app.current_model);
                                source.set_effort_bias(app.effort_memory.steps(&app.current_model));
                                tools_arc.set_vision_supported(model_sees_images(&source, &app.current_model));
                                let turn_opts = build_turn_options(&app.config, &app.current_effort);
                                app.turn_counter += 1;
                                app.turn_outcome = flashagent_core::TurnOutcome::default();
                                let (steer_tx, steer_rx) = tokio::sync::mpsc::unbounded_channel();
                                app.active_steer_tx = Some(steer_tx);
                                app.active_turn_handle = Some(spawn_turn(
                                    cancel.clone(),
                                    source.clone(),
                                    perm,
                                    app.history.clone(),
                                    GoalBudgets::steps_only(app.max_steps),
                                    turn_opts,
                                    tx.clone(),
                                    steer_rx,
                                    app.turn_counter,
                                ));
                            } else {
                                let now = std::time::Instant::now();
                                app.copy_toast = Some(("No previous turn to regenerate".to_string(), now));
                                app.renderer.request_reprint();
                            }
                        }
                    }

                    // F2: cycle reasoning expansion mode (none -> last -> all -> none)
                    KeyCode::F(2) => {
                        if !app.last_expanded && !app.all_expanded {
                            app.last_expanded = true;
                            app.all_expanded = false;
                        } else if app.last_expanded && !app.all_expanded {
                            app.last_expanded = false;
                            app.all_expanded = true;
                        } else {
                            app.last_expanded = false;
                            app.all_expanded = false;
                        }
                        app.renderer.request_reprint();
                    }

                    // ALT + O: expand / collapse ALL thinking blocks permanently (supports alternative keyboard layouts)
                    KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('\u{0449}') | KeyCode::Char('\u{0429}')
                        if mods.contains(KeyModifiers::ALT) =>
                    {
                        app.all_expanded = !app.all_expanded;
                        if app.all_expanded {
                            app.last_expanded = false;
                        }
                        app.renderer.request_reprint();
                    }

                    // CTRL + O: expand / collapse LAST thinking block temporarily (supports alternative keyboard layouts)
                    KeyCode::Char('o') | KeyCode::Char('O') | KeyCode::Char('\u{0449}') | KeyCode::Char('\u{0429}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        app.last_expanded = !app.last_expanded;
                        if app.last_expanded {
                            app.all_expanded = false;
                        }
                        app.renderer.request_reprint();
                    }

                    // CTRL + E: launch external editor on current input buffer
                    KeyCode::Char('e') | KeyCode::Char('E') | KeyCode::Char('\u{0443}') | KeyCode::Char('\u{0423}')
                        if mods.contains(KeyModifiers::CONTROL) && !app.running =>
                    {
                        match open_in_external_editor(&app.input, &app.config.external_editor) {
                            Ok(edited) => {
                                app.input = edited;
                                app.autocomplete_idx = 0;
                            }
                            Err(err) => {
                                notice!(&format!("Failed to launch external editor: {err}"));
                            }
                        }
                        app.renderer.request_reprint();
                    }

                    // CTRL + U: download & apply pending update or check for updates
                    KeyCode::Char('u') | KeyCode::Char('U') | KeyCode::Char('\u{0433}') | KeyCode::Char('\u{0413}')
                        if mods.contains(KeyModifiers::CONTROL) =>
                    {
                        if let Some((target_ver, asset_name, download_url, checksums_url)) = app.pending_update.clone() {
                            app.background = Some(BackgroundNotice::sticky(format!(
                                "{UPDATE_LINE_PREFIX}{target_ver} \u{b7} starting download..."
                            )));
                            app.renderer.request_reprint();
                            let update_tx_clone = update_tx.clone();
                            tokio::spawn(async move {
                                let progress_tx = update_tx_clone.clone();
                                let ver_for_progress = target_ver.clone();
                                let result = flashagent_svc::updater::download_and_apply_with_progress(
                                    &download_url,
                                    &asset_name,
                                    checksums_url.as_deref(),
                                    move |stage| {
                                        let _ = progress_tx.send(UpdateNotice::Progress {
                                            version: ver_for_progress.clone(),
                                            stage,
                                        });
                                    },
                                )
                                .await;
                                let notice = match result {
                                    Ok(_) => UpdateNotice::Ready { version: target_ver },
                                    Err(e) => UpdateNotice::Failed { error: e.to_string() },
                                };
                                let _ = update_tx_clone.send(notice);
                            });
                        } else if !flashagent_svc::updater::is_dev_mode() {
                            app.background = Some(BackgroundNotice::fading(
                                format!(
                                    "{UPDATE_LINE_PREFIX}\u{b7} checking the {} channel...",
                                    app.config.update_channel.label()
                                ),
                                30,
                            ));
                            app.renderer.request_reprint();
                            let update_tx_clone = update_tx.clone();
                            let ch = app.config.update_channel;
                            tokio::spawn(async move {
                                match flashagent_svc::updater::check_for_updates(ch, flashagent_svc::updater::DEFAULT_RELEASES_API).await {
                                    // Asked for by hand: go straight on to the
                                    // download instead of making the user press
                                    // Ctrl+U a second time.
                                    Ok(flashagent_svc::updater::UpdateStatus::UpdateAvailable { target, asset_name, download_url, checksums_url, .. }) => {
                                        let progress_tx = update_tx_clone.clone();
                                        let ver_for_progress = target.clone();
                                        let result = flashagent_svc::updater::download_and_apply_with_progress(
                                            &download_url,
                                            &asset_name,
                                            checksums_url.as_deref(),
                                            move |stage| {
                                                let _ = progress_tx.send(UpdateNotice::Progress {
                                                    version: ver_for_progress.clone(),
                                                    stage,
                                                });
                                            },
                                        )
                                        .await;
                                        let _ = update_tx_clone.send(match result {
                                            Ok(_) => UpdateNotice::Ready { version: target },
                                            Err(e) => UpdateNotice::Failed { error: e.to_string() },
                                        });
                                    }
                                    Ok(flashagent_svc::updater::UpdateStatus::UpToDate { current, .. }) => {
                                        let _ = update_tx_clone.send(UpdateNotice::UpToDate {
                                            version: current,
                                        });
                                    }
                                    Err(e) => {
                                        let _ = update_tx_clone.send(UpdateNotice::Failed { error: e.to_string() });
                                    }
                                }
                            });
                        } else {
                            app.background = Some(BackgroundNotice::fading("Auto-updater is disabled in dev mode", 6));
                            app.renderer.request_reprint();
                        }
                    }

                    // F4 or CTRL + T or ALT + T: open Thinking Effort menu (supports alternative keyboard layouts)
                    KeyCode::F(4)
                    | KeyCode::Char('t') | KeyCode::Char('T') | KeyCode::Char('\u{0435}') | KeyCode::Char('\u{0415}')
                        if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(4)) =>
                    {
                        let mut menu = build_effort_menu(&source, &app.effort_memory, &app.current_model);
                        menu.select_by_value(&app.current_effort);
                        app.effort_menu = Some(menu);
                    }

                    // F3 or CTRL + M or ALT + M: open Model menu (supports alternative keyboard layouts)
                    KeyCode::F(3)
                    | KeyCode::Char('m') | KeyCode::Char('M') | KeyCode::Char('\u{044c}') | KeyCode::Char('\u{042c}')
                        if mods.contains(KeyModifiers::CONTROL) || mods.contains(KeyModifiers::ALT) || matches!(code, KeyCode::F(3)) =>
                    {
                        if let Some(mut menu) = build_model_menu(&source) {
                            menu.select_by_value(&app.current_model);
                            app.model_menu = Some(menu);
                        } else {
                            notice!("[No models discovered from server]");
                        }
                    }

                    // F5: open Sampling Parameters menu
                    KeyCode::F(5) => {
                        if app.sampling_view.is_some() {
                            app.sampling_view = None;
                        } else {
                            app.effort_menu = None;
                            app.model_menu = None;
                            app.settings_view = None;
                            app.context_modal = None;
                            app.mcp_modal = None;
                            app.sampling_view = Some(SamplingView::new(&app.config));
                        }
                        app.renderer.request_reprint();
                    }

                    // Mode cycling with Shift+Tab (both KeyCode::BackTab and Tab+Shift)
                    KeyCode::BackTab | KeyCode::Tab if matches!(code, KeyCode::BackTab) || mods.contains(KeyModifiers::SHIFT) => {
                        let next_mode = perm.state().mode().next();
                        perm.state().set_mode(next_mode);
                        refresh_welcome_card_if_before_user_msg(
                            &mut app.chat,
                            &mut app.renderer,
                            &app.current_model,
                            &cwd_display,
                            next_mode.label(),
                            memory_docs,
                            &source,
                            &app.current_effort,
                            app.current_context.as_deref(),
                            app.config.show_mascot,
                            mascot_mood,
                        );
                        app.custom_placeholder = Some(format!("Permission mode set to: {}", next_mode.label()));
                        app.suggested_prompt = None;
                        app.renderer.request_reprint();
                    }
                    // Tab on empty input: toggle settings tab
                    KeyCode::Tab if app.input.is_empty() && gate.pending().is_none() && app.effort_menu.is_none() && app.model_menu.is_none() && app.sampling_view.is_none() && app.mcp_modal.is_none() => {
                        if app.settings_view.is_some() {
                            app.settings_view = None;
                        } else {
                            app.mcp_modal = None;
                            let runtime_mode = app.goal_state.as_ref().map_or(perm.state().mode(), |g| g.mode);
                            let effort = app.goal_state.as_ref().map_or(app.current_effort.as_str(), |g| g.effort.as_str());
                            app.settings_view = Some(settings_for_runtime(&app.config, runtime_mode, effort, &app.current_model, &app.available_models, app.context_usage.total_capacity));
                        }
                        app.renderer.request_reprint();
                    }
                    // Tab: complete autocomplete suggestion if input starts with `/`, or toggle approval choice when pending
                    KeyCode::Tab if gate.pending().is_some() => {
                        app.confirm_select.toggle();
                    }
                    KeyCode::Tab if app.input.starts_with('/') => {
                        if let Some(ac) = AutocompletePopup::for_input(&app.input, std::path::Path::new("."), app.autocomplete_idx) {
                            app.input = ac.complete_input(&app.input);
                            app.autocomplete_idx = 0;
                        }
                    }
                    // Arrow navigation for approval card, autocomplete popup, and prompt history
                    KeyCode::Left => {
                        if gate.pending().is_some() {
                            app.confirm_select.left();
                        }
                    }
                    KeyCode::Right => {
                        if gate.pending().is_some() {
                            app.confirm_select.right();
                        } else if !app.running && app.input.is_empty() {
                            if let Some(sug) = app.suggested_prompt.take() {
                                app.latest_suggestion = None;
                                app.input = sug;
                                app.renderer.request_reprint();
                            }
                        }
                    }
                    KeyCode::Up => {
                        if gate.pending().is_some() {
                            app.confirm_select.toggle();
                        } else if !app.running && app.input.starts_with('/') {
                            if let Some(ac) = AutocompletePopup::for_input(&app.input, std::path::Path::new("."), app.autocomplete_idx) {
                                if app.autocomplete_idx == 0 {
                                     app.autocomplete_idx = ac.items.len().saturating_sub(1);
                                } else {
                                     app.autocomplete_idx -= 1;
                                }
                            }
                        } else if !app.running && !app.input_history.is_empty() {
                            match app.history_index {
                                None => {
                                    app.current_draft = app.input.clone();
                                    let idx = app.input_history.len() - 1;
                                    app.history_index = Some(idx);
                                    app.input = app.input_history[idx].clone();
                                }
                                Some(idx) if idx > 0 => {
                                    let new_idx = idx - 1;
                                    app.history_index = Some(new_idx);
                                    app.input = app.input_history[new_idx].clone();
                                }
                                _ => {}
                            }
                        }
                    }
                    KeyCode::Down => {
                        if gate.pending().is_some() {
                            app.confirm_select.toggle();
                        } else if !app.running && app.input.starts_with('/') {
                            if let Some(ac) = AutocompletePopup::for_input(&app.input, std::path::Path::new("."), app.autocomplete_idx) {
                                app.autocomplete_idx = (app.autocomplete_idx + 1) % ac.items.len();
                            }
                        } else if !app.running {
                            if let Some(idx) = app.history_index {
                                if idx + 1 < app.input_history.len() {
                                    let new_idx = idx + 1;
                                    app.history_index = Some(new_idx);
                                    app.input = app.input_history[new_idx].clone();
                                } else {
                                    app.history_index = None;
                                    app.input = std::mem::take(&mut app.current_draft);
                                }
                            }
                        }
                    }
                    KeyCode::Backspace if gate.pending().is_none() => {
                        app.input.pop();
                        app.history_index = None;
                        app.autocomplete_idx = 0;
                        if app.input.is_empty() && app.latest_suggestion.is_some() {
                            app.suggested_prompt = app.latest_suggestion.clone();
                        }
                    }
                    KeyCode::Enter => {
                        app.custom_placeholder = None;
                        app.suggested_prompt = None;
                        app.latest_suggestion = None;
                        if gate.pending().is_some() {
                            gate.respond(app.confirm_select.decision());
                            app.confirm_select = ConfirmSelect::new();
                        } else if !app.input.is_empty() && app.running {
                            if let Some(ref steer_tx) = app.active_steer_tx {
                                let text = std::mem::take(&mut app.input);
                                if app.input_history.last() != Some(&text) {
                                    app.input_history.push(text.clone());
                                }
                                app.history_index = None;
                                app.current_draft.clear();
                                let _ = steer_tx.send(text);
                                app.renderer.request_reprint();
                            }
                        } else if !app.input.is_empty() && !app.running {
                            let mut cx = LoopCtx {
                                source: &source,
                                perm,
                                gate: &gate,
                                question_gate: &question_gate,
                                tools_arc: &tools_arc,
                                memory_block: &memory_block,
                                memory_docs,
                                cwd_display: &cwd_display,
                                cancel: &cancel,
                                tx: &tx,
                                rx: &mut rx,
                                update_tx: &update_tx,
                                channel_watch_tx: &channel_watch_tx,
                                session_id: &session_id,
                                mascot_mood,
                                tip_lines: &tip_lines,
                            };
                            match app.submit_input(&mut cx).await {
                                Flow::Continue => continue,
                                Flow::Quit => break 'main_loop,
                                Flow::Next => {}
                            }
                        }
                    }
                    KeyCode::Char(c)
                        if gate.pending().is_none()
                            && !mods.contains(KeyModifiers::CONTROL)
                            && !mods.contains(KeyModifiers::ALT) =>
                    {
                        app.input.push(c);
                        app.history_index = None;
                        app.autocomplete_idx = 0;
                    }
                    _ => {}
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
    fn the_labels_follow_the_language_in_both_directions() {
        // An English chat was labelled in Russian because the config said ru
        // and detection only ever switched one way.
        assert_eq!(conversation_language("Hi!"), Some("en"));
        assert_eq!(conversation_language("Привет!"), Some("ru"));
        assert_eq!(conversation_language("посмотри main.rs и скажи что там"), Some("ru"));
        assert_eq!(
            conversation_language("read main.rs and tell me what it does"),
            Some("en")
        );
        assert_eq!(conversation_language("ok"), Some("en"));
        assert_eq!(conversation_language("/help"), None, "a command says nothing about language");
        assert_eq!(conversation_language("42"), None);
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
        ] {
            assert_eq!(sanitize_user_suggestion(bad), None, "should have been dropped: {bad}");
        }
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

        // A tool shows what the model said it was for.
        assert_eq!(
            TurnPhase::Tool("Add the null check to parser.rs".into()).label(),
            "Add the null check to parser.rs"
        );
        // And a header long enough to break the line is shortened, not wrapped.
        let long = TurnPhase::Tool("x".repeat(200)).label();
        assert!(long.chars().count() <= 60, "{}", long.chars().count());
    }

    #[test]
    fn switching_channel_says_what_it_will_do() {
        use flashagent_core::config::UpdateChannel;

        // Going back to stable is a downgrade, and the user is told so in
        // those words before anything is replaced.
        let down = channel_switch_warning(
            UpdateChannel::Stable,
            "b238",
            &ChannelTarget::Version("v1.0.0".into()),
        );
        assert!(down.contains("from b238 down to v1.0.0"), "{down}");
        assert!(down.contains("disappear"), "{down}");
        assert!(down.ends_with("Continue?"), "{down}");

        let up = channel_switch_warning(
            UpdateChannel::Beta,
            "v1.0.0",
            &ChannelTarget::Version("b238".into()),
        );
        assert!(up.contains("from v1.0.0 to b238"), "{up}");
        assert!(up.contains("regress"), "{up}");

        // The feed has not answered yet, or could not be reached: name the
        // channel rather than invent a version.
        for unknown in [ChannelTarget::Checking, ChannelTarget::Unknown] {
            let text = channel_switch_warning(UpdateChannel::Stable, "b238", &unknown);
            assert!(text.contains("the newest stable release"), "{text}");
        }

        // Nothing published there yet — the honest answer is that you would
        // stay where you are.
        let empty = channel_switch_warning(UpdateChannel::Stable, "b238", &ChannelTarget::Empty);
        assert!(empty.contains("Nothing is published"), "{empty}");
        assert!(empty.contains("stay on b238"), "{empty}");
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
        let card = flashagent_tui::welcome_card_responsive_opts(
            "m", "/tmp", "Manual", 0, None, None, 100, 30, 0, true,
            flashagent_tui::MascotMood::Checking,
        );
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
}

