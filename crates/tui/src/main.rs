//! Terminal client: wires backend, loop, tools and permissions in-process and
//! renders through `flashagent_tui`.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyboardEnhancementFlags, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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
    visible_width, welcome_card, AutocompletePopup, ChatView, CompactionResult, ConfirmChoice, ConfirmSelect,
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
    write_in_text: flashagent_tui::Composer,
    is_writing: bool,
    selected_indices: std::collections::BTreeSet<usize>,
}

async fn cli_start(config: &mut AppConfig) -> Result<Option<(bool, bool, SessionStart)>> {
    let mut force_setup = false;
    let mut skip_trust = false;
    let mut session_start = SessionStart::New;
    let mut tool_test: Option<bool> = None;
let mut tool_test_scenario: Vec<String> = Vec::new();
let mut tool_test_json: Option<String> = None;
let mut tool_test_repeat: usize = 1;
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
            "--repeat" => {
                tool_test.get_or_insert(false);
                let Some(times) = args.next().and_then(|v| v.parse::<usize>().ok()) else {
                    anyhow::bail!("--repeat needs a number of runs, as in --repeat 3");
                };
                tool_test_repeat = times.clamp(1, 20);
            }
            "--scenario" => {
                tool_test.get_or_insert(false);
                let Some(key) = args.next() else { anyhow::bail!("--scenario needs a scenario key, as in --scenario long_stream") };
                tool_test_scenario.push(key.to_string());
            }
            "--json" => {
                tool_test.get_or_insert(false);
                let Some(path) = args.next() else { anyhow::bail!("--json needs a file to write the results to") };
                tool_test_json = Some(path.to_string());
            }
            "--setup" => force_setup = true,
            "-y" | "--yes" => skip_trust = true,
            "-h" | "--help" => {
                println!("FlashAgent TUI\n\nUsage: flashagent [OPTIONS]\n\nOptions:\n  -v, --version        Print version\n  --update             Check and apply updates\n  --channel <name>     Switch release channel (stable, beta)\n  --model <name>       Model to use, saved for the provider in use\n  --url <endpoint>     Talk to this server for this run only (saved providers: /provider)\n  --setup              Run first-time setup wizard\n  --tool-test [--all-models]  Check whether the model can drive tools
      --scenario <key>  Check one scenario (repeatable)
      --json <file>      Write the full results as JSON to a file\n  -r, --resume [id]    Resume a saved session (without an id: pick one from this folder)\n  -c, --continue       Continue the latest session in this folder\n  -y, --yes            Skip directory trust confirmation\n  --uninstall [-y]     Remove FlashAgent; asks what data to delete (-y: take the defaults)\n  -h, --help           Show this help message");
                return Ok(None);
            }
            other => anyhow::bail!("usage: flashagent [-v] [--update] [--channel <stable|beta>] [--model <name>] [--url http://host/v1] [--tool-test [--all-models] [--scenario <key>] [--json <file>]] [--setup] [-r|--resume [id]] [-c|--continue] [-y|--yes] (got {other})"),
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
        let code = run_tool_check_cli(config, all_models, tool_test_scenario, tool_test_json, tool_test_repeat).await;
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
    // A run that already knows where to go does not get asked where that is.
    // `--url` and `--model` answer the wizard's questions before it opens, and
    // asking a person who has just typed both to type them again in a form is
    // the wizard being noticed rather than being needed.
    let already_answered = !force_setup && config.is_ready_to_run(|k| std::env::var(k).ok());
    if (!config.setup_completed || force_setup) && !already_answered && std::io::stdout().is_terminal() {
        let completed = flashagent_tui::run_wizard(config).await.unwrap_or(false);
        if !completed {
            return None;
        }
        // Nothing new to show: stamp the version so the next update has a baseline.
        config.last_seen_version = Some(flashagent_svc::updater::current_version().to_string());
        let _ = config.save();
        ran_setup = true;
    } else if already_answered && !config.setup_completed {
        // Skipped, so the next launch must not ask either.
        config.setup_completed = true;
        let _ = config.save();
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
    // Half a second is right for a server on this machine, where the answer is
    // already in memory, and badly wrong for one across the internet:
    // OpenRouter lists hundreds of models and answers in about two seconds, so
    // the app used to give up, carry on knowing nothing ("Context: not known
    // yet") and leave the mascot saying the server was dead while it was
    // answering perfectly well. The wait is bounded by discovery's own timeout
    // either way, so a server that is truly gone still reaches the prompt.
    let remote =
        flashagent_core::url_host(&endpoint.url).is_some_and(|host| !flashagent_core::is_local_host(&host));
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

    // How long startup waits for the server to say what it runs. Half a second is
    // right for a server on this machine, where the answer is already in memory,
    // and badly wrong for one across the internet: OpenRouter lists hundreds of
    // models and answers in about two seconds, so the app used to give up, carry
    // on knowing nothing ("Context: not known yet") and leave the mascot saying
    // the server was dead while it was answering perfectly well. The wait is
    // bounded by discovery's own timeout either way, so a server that is truly
    // gone still gets to the prompt on its own.
    let startup_timeout = if remote {
        std::time::Duration::from_secs(15)
    } else if model.is_empty() {
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

/// Set when tmux was asked for xterm's `modifyOtherKeys`, so the matching
/// reset goes out on the way back to the shell and not before.
static TMUX_MODIFIED_KEYS: AtomicBool = AtomicBool::new(false);

/// True when tmux says modified keys are reported at all and that the report
/// arrives in the `csi-u` form. tmux's `xterm` form is `CSI 27;mod;key~`,
/// which crossterm does not parse, so it must not be asked for.
fn tmux_forwards_extended_keys(report: &str) -> bool {
    let mut fields = report.split_whitespace();
    let keys = fields.next().unwrap_or_default();
    let format = fields.next().unwrap_or_default();
    matches!(keys, "on" | "always") && format == "csi-u"
}

/// tmux up to 3.8 does not know the kitty protocol: it reports a modified key
/// only after an application asks for xterm's `modifyOtherKeys` mode, and only
/// when its own options say to. Asked for only when the report will be in the
/// `csi-u` form, which reaches the reader as the same `CSI … u` sequence a
/// kitty-aware terminal sends.
fn ask_tmux_for_modified_keys() -> bool {
    if std::env::var_os("TMUX").is_none() {
        return false;
    }
    let Ok(out) = std::process::Command::new("tmux")
        .args(["display-message", "-p", "#{extended-keys} #{extended-keys-format}"])
        .stdin(std::process::Stdio::null())
        .output()
    else {
        return false;
    };
    if !tmux_forwards_extended_keys(&String::from_utf8_lossy(&out.stdout)) {
        return false;
    }
    use std::io::Write;
    let _ = std::io::stdout().write_all(b"\x1b[>4;2m");
    let _ = std::io::stdout().flush();
    true
}

fn release_tmux_modified_keys() {
    if !TMUX_MODIFIED_KEYS.swap(false, Ordering::Relaxed) {
        return;
    }
    use std::io::Write;
    let _ = std::io::stdout().write_all(b"\x1b[>4;0m");
    let _ = std::io::stdout().flush();
}

/// Shift+Enter reaches an application as plain Enter unless the terminal
/// speaks the kitty keyboard protocol. `DISAMBIGUATE_ESCAPE_CODES` is enough
/// for that: the terminal then reports Enter, Tab and Backspace as
/// `CSI … u` with the shift in the modifier field, and leaves printable keys
/// as the text they are.
///
/// `REPORT_ALL_KEYS_AS_ESCAPE_CODES` is deliberately not used. It makes the
/// terminal send every key as `CSI key-code;modifiers u` with the *unshifted*
/// key code, and the shifted character only in a second field that crossterm
/// 0.28 does not read. The result was that `~` arrived as a backtick with a
/// shift modifier, and Caps Lock, which the terminal reports the same way,
/// stopped working: the typed character came out unshifted.
fn enable_enhanced_keys() {
    let _ = crossterm::execute!(
        std::io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    TMUX_MODIFIED_KEYS.store(ask_tmux_for_modified_keys(), Ordering::Relaxed);
}

/// Pops one level of the flags pushed above, so a terminal that shares its
/// screen with the shell does not keep the enhanced encoding after exit.
fn restore_enhanced_keys() {
    release_tmux_modified_keys();
    let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
}

fn install_crash_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        flashagent_tui::screen::set_progress(flashagent_tui::screen::Progress::None);
        restore_enhanced_keys();
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
            restore_enhanced_keys();
            let _ = crossterm::execute!(std::io::stdout(), crossterm::cursor::Show, DisableMouseCapture, DisableBracketedPaste, LeaveAlternateScreen);
            let _ = crossterm::terminal::disable_raw_mode();
            std::process::exit(code);
        });
    }
    // The built-in toolset plus `spawn_agent`. The two channels are made here,
    // before the toolset that needs them, and the app's event loop reads them:
    // a running child's steps go to the screen, and a finished child's answer
    // becomes a notice the model reads without anyone asking for it.
    let (agent_tx, agent_events) = tokio::sync::mpsc::unbounded_channel();
    let (composite, agent_finished) =
        flashagent_tools::agent_tools(tools_arc.clone(), source.clone(), state.clone(), Some(agent_tx));
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
        agent_events,
        agent_finished,
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
    enable_enhanced_keys();
    let result = run_app(ctx).await;

    flashagent_tui::screen::set_progress(flashagent_tui::screen::Progress::None);
    // Before the screen goes: the shell below must get plain Enter and Tab back.
    restore_enhanced_keys();
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

/// A switch made in the app that the transcript has not announced yet. The
/// rule belongs to the first message that uses the new state: a menu pick
/// alone must not put a rule above an empty chat.
#[derive(Default)]
struct PendingSwitch {
    /// The provider name, when the change came from `/provider`.
    provider: Option<String>,
    model: bool,
    effort: bool,
}

impl PendingSwitch {
    /// One rule for the changes waiting together: a provider switch already
    /// carries the model and effort it runs, so it must not be split in two.
    fn label(&self, model: &str, effort: &str) -> Option<String> {
        if let Some(provider) = &self.provider {
            Some(format!("Provider: {provider} \u{b7} {model} \u{b7} thinking effort: {effort}"))
        } else if self.model {
            Some(format!("Model: {model} \u{b7} thinking effort: {effort}"))
        } else if self.effort {
            Some(format!("Thinking effort: {effort}"))
        } else {
            None
        }
    }
}

struct App {
    question_ui_state: QuestionUiState,
    history: Vec<ChatMessage>,
    /// The history is with the compaction task until it comes back.
    compacting: bool,
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
    /// The call whose whole diff `v` already put in the conversation.
    whole_change_shown: Option<String>,
    /// The last look at the server got no answer; cleared when one comes.
    server_silent: bool,
    /// Ctrl+X, waiting for Ctrl+E.
    ctrl_x_at: Option<std::time::Instant>,
    chat: ChatView,
    /// A model/provider/effort change not yet written to the transcript.
    pending_switch: PendingSwitch,
    running: bool,
    active_turn_handle: Option<tokio::task::JoinHandle<()>>,
    /// The running turn's own history, held here so a save taken while it runs
    /// writes what it has done so far rather than the conversation as it stood
    /// when the turn started. `None` when no turn is running.
    turn_history: Option<flashagent_core::loop_::SharedHistory>,
    /// The mid-turn compactor's last result, shared with the turn. The core
    /// event says how much was saved; the card needs the summary too.
    mid_turn_report: Option<Arc<std::sync::Mutex<Option<flashagent_tui::CompactionResult>>>>,
    active_steer_tx: Option<tokio::sync::mpsc::UnboundedSender<flashagent_core::Steer>>,
    pending_steers: Vec<(String, Vec<Attachment>)>,
    /// Commands typed while a turn ran that change the conversation: they run when it ends.
    queued_commands: Vec<String>,
    /// Notices of background tasks that ended, on their way to the model.
    task_inbox: flashagent_core::NoticeInbox,
    /// Transcript lines of tasks that ended while a turn was writing.
    task_lines: Vec<String>,
    tasks_refreshed: std::time::Instant,
    /// The subagents of this session: one row each, live, and the notices
    /// their answers become.
    agents: flashagent_tui::agents::AgentTree,
    /// Subagent rows the mouse has opened. F2 does not touch these: a child's
    /// row is a status above the composer, not a line of the transcript.
    expanded_agents: std::collections::HashSet<String>,
    /// Where a mouse selection started, in (row, column). A drag from here is
    /// copied as it is made.
    selection_anchor: Option<(u16, u16)>,
    /// What a wheel throw still has left to spend: a trackpad's flick arrives as
    /// a burst of small events and would otherwise stop dead at the end of it.
    wheel_momentum: i32,
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
    /// What this session has cost, where the backend says. Unpriced backends
    /// leave it empty and the status bar stays quiet.
    cost: flashagent_core::CostLedger,
    /// The model's own checklist for whatever it is doing, in any session and
    /// not only under /goal.
    plan: Vec<flashagent_tools::plan_tool::PlanStep>,
    /// The arguments of the call now running, so a finished call can be read
    /// back without the loop having to carry them a second time.
    last_tool_args: String,
    /// Kept so the prompt can be rebuilt when the voice changes.
    prompt_config: SystemPromptConfig,
    /// A `/goal` run is under way: the rules that only hold inside one belong in
    /// the prompt, and they are not there the rest of the time.
    goal_active: bool,
    /// What the system prompt measured this session. The tool schemas are given
    /// what the context window has left after this, so it is kept here rather
    /// than measured again at every model switch.
    prompt_tokens: usize,
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
    /// Unprompted things (an update, a notice) go on the line under the input.
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
    /// /update; until then a background update goes unannounced.
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
/// When the running turn last put its work on disk. A turn writes the whole
/// conversation, so writing it on every step would be quadratic; the gaps keep
/// the cost flat while still bounding how much a crash can lose.
last_autosave: Option<std::time::Instant>,
    /// How long the turn that just ended took. Read after `turn_started` is
    /// cleared, so it has to be kept: the only way to tell a model that is
    /// overthinking from one that is right on time is how long it took, and
    /// without this the answer is never None-forever.
    turn_elapsed: Option<f32>,
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
pub(crate) const OFFLINE_NOTICE: &str = "No answer from";

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
    agent_events: tokio::sync::mpsc::UnboundedReceiver<flashagent_core::SubagentEvent>,
    /// What running subagents report, and everything the rest of the task says:
    /// a child that answered, a message between agents, a review of a report.
    agent_finished: tokio::sync::mpsc::UnboundedReceiver<flashagent_core::SubagentOutbound>,
}

/// Set while an external editor has the terminal; the key reader waits.
static INPUT_PAUSED: AtomicBool = AtomicBool::new(false);

/// The editor setting, or `$VISUAL`, `$EDITOR` and the platform's own. The
/// default setting is the text `$EDITOR`, which is not a program: run as one,
/// The external editor failed on every fresh config.
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
    // The editor is a normal program: hand its Enter and Tab back before it starts.
    restore_enhanced_keys();
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
    enable_enhanced_keys();

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

/// The character a key press means when the terminal reports the key and the
/// shift apart: `~` is a backtick with shift, `!` is a one with shift. A
/// terminal that sends the character itself is not touched, and a character
/// that is already shifted stays as it is.
fn shifted_char(c: char, mods: KeyModifiers) -> char {
    if !mods.contains(KeyModifiers::SHIFT) {
        return c;
    }
    match c {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        '`' => '~',
        _ => c.to_uppercase().next().unwrap_or(c),
    }
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
        KeyCode::Char(c) if plain => Some(typed_case(c, k)),
        KeyCode::Char(c) if alt_gr && !c.is_ascii_alphanumeric() && !c.is_control() => Some(c),
        KeyCode::Enter if k.modifiers.is_empty() => Some('\n'),
        _ => None,
    }
}

/// The character as it is meant: the shift a terminal reports apart from the
/// key is applied here, and Caps Lock, which a terminal reports the same way,
/// turns a letter into its capital.
fn typed_case(c: char, k: &crossterm::event::KeyEvent) -> char {
    let shifted = shifted_char(c, k.modifiers);
    if k.state.contains(crossterm::event::KeyEventState::CAPS_LOCK) {
        return shifted.to_uppercase().next().unwrap_or(shifted);
    }
    shifted
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

fn start_event_sources(tx: &tokio::sync::mpsc::UnboundedSender<UiEvent>, tools_arc: &Arc<BuiltinTools>, pending_discovery: Option<tokio::task::JoinHandle<Option<flashagent_llm::ServerDiscovery>>>, url: String) {
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
            let _ = tx_disc.send(match task.await {
                Ok(Some(disc)) => UiEvent::ServerDiscovered(disc),
                _ => UiEvent::ServerSilent(url),
            });
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
    // Measured before the text moves into the history: what the schemas are
    // given is what the prompt actually costs, not what its ceiling allows.
    let prompt_tokens = flashagent_llm::count_tokens(&model, &system_prompt_text);
    App {
        question_ui_state: QuestionUiState::default(),
        history: vec![ChatMessage::system(system_prompt_text)],
        compacting: false,
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
        whole_change_shown: None,
        ctrl_x_at: None,
        chat: ChatView::default(),
        pending_switch: PendingSwitch::default(),
        running: false,
        active_turn_handle: None,
        turn_history: None,
        mid_turn_report: None,
        active_steer_tx: None,
        pending_steers: Vec::new(),
        queued_commands: Vec::new(),
        task_inbox: flashagent_core::NoticeInbox::default(),
        agents: flashagent_tui::agents::AgentTree::default(),
    expanded_agents: std::collections::HashSet::new(),
    selection_anchor: None,
    wheel_momentum: 0,
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
        cost: flashagent_core::CostLedger::default(),
        plan: Vec::new(),
        last_tool_args: String::new(),
        prompt_config: system_prompt_config,
        goal_active: false,
        prompt_tokens,
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
        last_autosave: None,
        turn_elapsed: None,
        max_steps: app_config.max_steps,
        goal_state: None,
        goal_ledger: None,
        copy_toast: None,
        last_ctrl_c: None,
        last_mascot_mood: MascotMood::Checking,
        announced_mood: MascotMood::Checking,
        server_silent: false,
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
    // Start-up already asked, and was told nothing.
    let startup_silent = ctx.pending_discovery.is_none() && ctx.available_models.is_empty();
    start_event_sources(&tx, &ctx.tools_arc, ctx.pending_discovery, ctx.source.0.base_url());
    // A running subagent reports into its own channel; both are read here and
    // handed to the loop as its own events, so a child's steps reach the screen
    // and its answer, its messages and the reviews reach the model.
    {
        let (tx_events, mut events) = (tx.clone(), ctx.agent_events);
        let (tx_done, mut done) = (tx.clone(), ctx.agent_finished);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Some(event) => {
                            if tx_events.send(UiEvent::SubagentEvent(event)).is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                    finished = done.recv() => match finished {
                        Some(finished) => {
                            if tx_done.send(UiEvent::SubagentOutbound(finished)).is_err() {
                                return;
                            }
                        }
                        None => return,
                    },
                }
            }
        });
    }
    let (channel_probe_tx, channel_probes) = tokio::sync::mpsc::unbounded_channel::<ChannelTarget>();
    let (update_tx, updates) = tokio::sync::mpsc::unbounded_channel::<UpdateNotice>();
    let (channel_watch_tx, channel_watch_rx) = tokio::sync::watch::channel(ctx.config.update_channel);
    // The background updater and /update both claim this, so they never download
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
        .with_effort(&ctx.initial_effort)
        // `spawn_agent` is always in the composite, so the rules about a
        // subagent's report belong in the prompt.
        .with_subagents(true);
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
    app.server_silent = startup_silent;
    update_context_usage(&app.current_model, &mut app.context_usage, &app.history, &w.memory_block, &app.chat, w.perm);
    // Before the warm-up: it exists to prefill exactly the prefix the first turn
    // will send, and a system message that gains a line after it went out is no
    // longer that prefix.
    app.refresh_context_line();

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
    if let Some(id) = resume_session_id.as_deref() {
        if app.resume_at_start(id, &w.memory_block, w.perm) {
            app.tell_of_lost_tasks(&w.tools_arc.shells().tasks());
        } else {
            session_id = new_session_id();
            open_snapshots(w.perm, &session_id, &w.cwd);
        }
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
                app.tell_of_lost_tasks(&w.tools_arc.shells().tasks());
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
    model: &str,
    usage: &mut ContextUsage,
    history: &[ChatMessage],
    memory_block: &str,
    chat: &ChatView,
    tools: &dyn flashagent_core::ToolExec,
) {
    // One counter for the model in use, everywhere. This used to be a fourth
    // of the character count while the status line counted with a real
    // tokenizer and the budget with a third rule, so the bar, the budget and
    // the speed readout could each be a different number for the same text.
    let count = |text: &str| flashagent_llm::count_tokens(model, text);
    usage.system_tokens = history
        .iter()
        .filter(|m| m.role == flashagent_llm::Role::System)
        .map(|m| count(&m.content))
        .sum();
    usage.memory_tokens = count(memory_block);
    // Every advertised schema, MCP included.
    usage.tools_tokens = tools
        .specs()
        .iter()
        .map(|t| count(&t.name) + count(&t.description) + count(&t.parameters_json))
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

    // The conversation itself is left on the character estimate on purpose: it
    // is recounted on every keystroke and every token of it, and a server that
    // reports usage corrects the total anyway. What the budget is decided on is
    // the three above, and those are counted with the model's own tokenizer.
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
            // A level this build never heard of. `custom_effort` is what the
            // request carries, so the typed intent must not contradict it:
            // leaving it `Auto` would let any path reading `thinking` alone
            // quietly override the user's level.
            _ => opts.thinking = flashagent_llm::ThinkingEffort::Default,
        }
    }
    opts
}

#[allow(clippy::too_many_arguments)]
fn spawn_turn(
    cancel: Arc<std::sync::atomic::AtomicBool>,
    source: Arc<BackendSource>,
    perm: &'static PermissionedTools,
    history: flashagent_core::loop_::SharedHistory,
    budgets: GoalBudgets,
    turn_opts: flashagent_llm::TurnOptions,
    tx: tokio::sync::mpsc::UnboundedSender<UiEvent>,
    steer_rx: tokio::sync::mpsc::UnboundedReceiver<flashagent_core::Steer>,
    turn_id: u64,
    voice_prelude: Vec<ChatMessage>,
    compactor: Option<Arc<TurnCompactor>>,
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
        let mut loop_ = AgentLoop::with_steering(config, cancel, steer_rx);
        if let Some(compactor) = compactor {
            loop_ = loop_.with_compactor(compactor);
        }
        let res = loop_
            .run(source.as_ref(), perm, history, |event| {
                let _ = tx.send(UiEvent::Loop { turn_id, event });
            })
            .await;
        let result = res.map_err(|e| (e.to_string(), e.into_history())).map(|(shared, done)| (shared.snapshot(), done));
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
    use flashagent_core::Compactor;

    #[test]
    fn tmux_extended_keys_are_used_only_in_the_csi_u_format() {
        assert!(tmux_forwards_extended_keys("on csi-u\n"));
        assert!(tmux_forwards_extended_keys("always csi-u"));
        assert!(!tmux_forwards_extended_keys("on xterm\n"), "the xterm form is not parsed by the reader");
        assert!(!tmux_forwards_extended_keys("off csi-u\n"), "tmux would not forward it anyway");
        assert!(!tmux_forwards_extended_keys(""), "no answer means no request");
    }

    #[test]
    fn a_shift_reported_apart_from_the_key_is_the_shifted_character() {
        let shift = KeyModifiers::SHIFT;
        assert_eq!(shifted_char('`', shift), '~');
        assert_eq!(shifted_char('1', shift), '!');
        assert_eq!(shifted_char('a', shift), 'A');
        assert_eq!(shifted_char('~', shift), '~', "an already shifted character stays as it is");
        assert_eq!(shifted_char('a', KeyModifiers::NONE), 'a');
    }

    #[test]
    fn a_terminal_that_reports_the_key_and_the_shift_apart_types_the_shifted_character() {
        // A kitty-style terminal that reports the key code and the shift
        // separately: `~` arrived as a backtick and Caps Lock as a lower case
        // letter.
        let tilde = crossterm::event::KeyEvent::new(KeyCode::Char('`'), KeyModifiers::SHIFT);
        assert_eq!(typed_char(&tilde), Some('~'));
        let caps = crossterm::event::KeyEvent::new_with_kind_and_state(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
            crossterm::event::KeyEventState::CAPS_LOCK,
        );
        assert_eq!(typed_char(&caps), Some('A'), "Caps Lock must still make a capital");
        let plain = crossterm::event::KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(typed_char(&plain), Some('a'));
    }

    #[test]
    fn a_pending_switch_becomes_one_rule() {
        let mut p = PendingSwitch::default();
        assert_eq!(p.label("m", "auto"), None);
        p.model = true;
        assert_eq!(p.label("m", "high").as_deref(), Some("Model: m · thinking effort: high"));
        p.provider = Some("Lab".into());
        assert_eq!(p.label("m", "high").as_deref(), Some("Provider: Lab · m · thinking effort: high"));
        let p = PendingSwitch { effort: true, ..Default::default() };
        assert_eq!(p.label("m", "high").as_deref(), Some("Thinking effort: high"));
    }

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
    fn a_turn_that_took_time_leaves_the_time_behind_when_its_clock_stops() {
        // The duration used to be read after the clock was cleared, so it came
        // back None every time: nothing learned from a slow turn, live, while
        // every test that handed a duration in by hand stayed green.
        let mut started = Some(std::time::Instant::now() - std::time::Duration::from_secs(3));
        let took = turns::elapsed(&mut started);
        assert!(took.is_some_and(|t| (t - 3.0).abs() < 0.05), "the time the turn took is kept, got {took:?}");
        assert_eq!(started, None, "and the clock is stopped, in the same step");
        // A clock that was never started cannot report a time.
        assert_eq!(turns::elapsed(&mut started), None);
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

    const SHAPED_SUMMARY: &str = "## Objective\nfirst question\n\n## Requirements\n- none\n\n## Decisions\n- none\n\n## Work State\n### Completed\n- read a file\n### Active\n- none\n### Blocked\n- none\n\n## Next Move\n1. continue the work\n\n## Relevant Files\n- none\n\n## Important Context\n- none";

    struct ScriptedCompaction(flashagent_llm::FinishReason);

    #[async_trait::async_trait]
    impl LlmSource for ScriptedCompaction {
        async fn turn(
            &self,
            _messages: &[ChatMessage],
            _tools: &[flashagent_llm::ToolSpec],
        ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
            let events = vec![
                Ok(flashagent_llm::LlmEvent::TextDelta(SHAPED_SUMMARY.into())),
                Ok(flashagent_llm::LlmEvent::Done(self.0)),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// A model that skipped a section is not refused: what it wrote is kept,
    /// with a note about what is missing. Free models do this, and a window
    /// that stays full helps nobody.
    struct LooseCompaction;

    #[async_trait::async_trait]
    impl LlmSource for LooseCompaction {
        async fn turn(
            &self,
            _messages: &[ChatMessage],
            _tools: &[flashagent_llm::ToolSpec],
        ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
            let events = vec![
                Ok(flashagent_llm::LlmEvent::TextDelta("## Objective\nfix the parser\n\n## Next Move\n1. read the file".into())),
                Ok(flashagent_llm::LlmEvent::Done(flashagent_llm::FinishReason::Stop)),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn a_summary_that_skipped_a_section_is_kept_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("conversation.jsonl");
        let mut history = long_history();
        let result = compact_context(&LooseCompaction, &mut history, None, &archive, None)
            .await
            .expect("the text the model wrote is kept even without the shape");
        assert!(result.summary.contains("## Objective"), "{}", result.summary);
        assert!(result.summary.contains("did not take the usual shape"), "the shape is said out loud: {}", result.summary);
        assert!(result.summary.contains("Requirements"), "the missing section is named: {}", result.summary);
        assert!(result.summary.contains("## Next Move"), "what the model wrote is kept: {}", result.summary);
    }

    #[tokio::test]
    async fn a_single_exchange_can_be_compacted_too() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("conversation.jsonl");
        let mut history = vec![
            ChatMessage::system("SYSTEM PROMPT"),
            ChatMessage::user("fix the parser"),
            ChatMessage::assistant("here it is"),
        ];
        // A compaction the person asked for: it happens even when the summary
        // is longer than the exchange it replaces.
        let result = compact_context_with_sink(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, None, &archive, None, None, true)
            .await
            .expect("one exchange is enough to compact");
        assert!(history[0].content.contains("Compacted Conversation History"), "{}", history[0].content);
        assert!(history[0].content.contains("longer than the conversation"), "the growth is said out loud: {}", history[0].content);
        assert_eq!(history[1].content, "fix the parser", "the task stays: the model still has it");
        assert_eq!(history.len(), 2, "the exchange is replaced by the summary: {}", history.len());
        assert_eq!(result.saved, 0, "nothing was freed, and the card says so");
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
        let freed = compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, None, &archive, None).await;
        assert!(freed.is_some());
        assert_eq!(history.iter().filter(|m| m.role == flashagent_llm::Role::System).count(), 1);
        assert!(history[0].content.starts_with("SYSTEM PROMPT"));
        assert!(history[0].content.contains("The summary below covers the earlier conversation.\n## Objective"));
        assert!(history[0].content.contains("Continue the current task from this summary"));
        assert!(history[0].content.contains(&archive.display().to_string()));
        assert!(history[0].content.contains(crate::compact::COMPACTION_STATS_MARK.trim()), "the card's bill is stored for a resume");
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
        assert!(compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, None, &archive, None).await.is_some());
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

    /// The same shaped summary in two pieces, so a test can see the card get
    /// the first one before the call that writes the second has finished.
    struct SplitCompaction;

    #[async_trait::async_trait]
    impl LlmSource for SplitCompaction {
        async fn turn(
            &self,
            _messages: &[ChatMessage],
            _tools: &[flashagent_llm::ToolSpec],
        ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
            let (first, second) = SHAPED_SUMMARY.split_at(SHAPED_SUMMARY.find("## Requirements").unwrap());
            let events = vec![
                Ok(flashagent_llm::LlmEvent::TextDelta(first.into())),
                Ok(flashagent_llm::LlmEvent::TextDelta(second.into())),
                Ok(flashagent_llm::LlmEvent::Done(flashagent_llm::FinishReason::Stop)),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn a_compaction_summary_reaches_the_card_piece_by_piece() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("c.jsonl");
        let mut history = long_history();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let sink = SummarySink::ui(tx);
        let result = compact_context_with_sink(&SplitCompaction, &mut history, None, &archive, None, Some(&sink), true)
            .await
            .expect("the summary is accepted");
        assert!(result.summary.contains("## Objective"));

        let mut pieces = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let UiEvent::CompactionDelta(text) = event {
                pieces.push(text);
            }
        }
        assert_eq!(pieces.len(), 2, "both streamed pieces reached the card: {pieces:?}");
        assert_eq!(pieces.concat(), SHAPED_SUMMARY);
    }

    /// Answers with `first` on the first call and `second` after that, and
    /// counts the calls, so a test can see whether a second ask was made.
    struct TwoShapeSummaries {
        first: &'static str,
        second: &'static str,
        calls: std::sync::Mutex<usize>,
    }

    #[async_trait::async_trait]
    impl LlmSource for TwoShapeSummaries {
        async fn turn(
            &self,
            _messages: &[ChatMessage],
            _tools: &[flashagent_llm::ToolSpec],
        ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            let text = if *calls == 1 { self.first } else { self.second };
            let events = vec![
                Ok(flashagent_llm::LlmEvent::TextDelta(text.into())),
                Ok(flashagent_llm::LlmEvent::Done(flashagent_llm::FinishReason::Stop)),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// A conversation long enough to be worth compacting.
    fn long_history() -> Vec<ChatMessage> {
        vec![
            ChatMessage::system("SYSTEM PROMPT"),
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer".repeat(1000)),
            ChatMessage::user("read a file"),
            ChatMessage::assistant("here it is"),
        ]
    }

    #[tokio::test]
    async fn a_summary_without_the_expected_shape_is_asked_for_once_more_and_kept_afterwards() {
        // Seen live: a model that reads and answers well wrote a plain summary,
        // sections 7 and 8 missing, and the window stayed full behind a message
        // that said only that it had failed.
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("c.jsonl");
        let shapeless = "Summary:\nThe user asked a question and I answered it.";
        let shaped = SHAPED_SUMMARY;

        let mut history = long_history();
        let src = TwoShapeSummaries { first: shapeless, second: shaped, calls: std::sync::Mutex::new(0) };
        assert!(compact_context(&src, &mut history, None, &archive, None).await.is_some(), "the second ask is taken");
        assert_eq!(*src.calls.lock().unwrap(), 2, "and it is the last one");
        assert!(history[0].content.contains("continue the work"));

        // Asked twice and the shape is still not there: what the model wrote is
        // kept, with a note about what is missing. The rule is not relaxed
        // into refusing everything, and the window is freed either way.
        let mut history = long_history();
        let src = TwoShapeSummaries { first: shapeless, second: shapeless, calls: std::sync::Mutex::new(0) };
        let kept = compact_context(&src, &mut history, None, &archive, None)
            .await
            .expect("the text the model wrote is kept");
        assert_eq!(*src.calls.lock().unwrap(), 2, "no third ask");
        assert!(kept.summary.contains("The user asked a question"), "what it wrote is kept: {}", kept.summary);
        assert!(kept.summary.contains("did not take the usual shape"), "the shape is said out loud: {}", kept.summary);
        assert!(kept.summary.contains("Requirements"), "the missing section is named: {}", kept.summary);

        // A first answer that already has the shape is not asked for again.
        let mut history = long_history();
        let src = TwoShapeSummaries { first: shaped, second: shapeless, calls: std::sync::Mutex::new(0) };
        assert!(compact_context(&src, &mut history, None, &archive, None).await.is_some());
        assert_eq!(*src.calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn plain_headings_are_rewritten_into_the_card_shape() {
        // A model that answers with "Objective:" rather than "## Objective" is
        // not refused over punctuation: the stored summary gets the headings the
        // card expects, and the model still receives every required section.
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("c.jsonl");
        let plain = "Here is the summary:\n\nObjective:\ncontinue the work\n\nRequirements:\n- none\n\nDecisions:\n- none\n\nWork State:\nCompleted:\n- first question answered\nActive:\n- none\nBlocked:\n- none\n\nNext Move:\n1. ship\n\nRelevant Files:\n- none\n\nImportant Context:\n- none";
        let mut history = long_history();
        let src = TwoShapeSummaries { first: plain, second: "nope", calls: std::sync::Mutex::new(0) };
        let result = compact_context(&src, &mut history, None, &archive, None).await.expect("plain headings are accepted after normalization");
        assert!(result.summary.starts_with("## Objective"), "{}", result.summary);
        assert!(result.summary.contains("### Completed"), "{}", result.summary);
        assert_eq!(*src.calls.lock().unwrap(), 1, "it was not asked a second time");
        assert!(history[0].content.contains("## Objective"));
    }

    #[test]
    fn a_session_summary_is_found_in_the_system_message_for_the_card() {
        let content = format!(
            "SYSTEM PROMPT{}{}## Objective\nship it{}/tmp/a.jsonl\nContinue{}17 in · 5 out",
            crate::compact::COMPACTED_MARK,
            crate::compact::COMPACTED_INTRO,
            crate::compact::ARCHIVE_MARK,
            crate::compact::COMPACTION_STATS_MARK
        );
        assert_eq!(crate::compact::summary_from_system(&content), Some("## Objective\nship it"));
        assert_eq!(crate::compact::stats_from_system(&content), Some((17, 5)));
        assert_eq!(crate::compact::summary_from_system("SYSTEM PROMPT"), None);
        assert_eq!(crate::compact::stats_from_system("SYSTEM PROMPT"), None);
    }

    #[tokio::test]
    async fn compaction_usage_after_the_finish_reason_is_still_counted() {
        struct LateUsage;
        #[async_trait::async_trait]
        impl LlmSource for LateUsage {
            async fn turn(
                &self,
                _messages: &[ChatMessage],
                _tools: &[flashagent_llm::ToolSpec],
            ) -> Result<futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>, flashagent_llm::LlmError> {
                let events = vec![
                    Ok(flashagent_llm::LlmEvent::TextDelta(SHAPED_SUMMARY.into())),
                    Ok(flashagent_llm::LlmEvent::Done(flashagent_llm::FinishReason::Stop)),
                    Ok(flashagent_llm::LlmEvent::Usage(flashagent_llm::Usage {
                        prompt: Some(90_000),
                        completion: Some(5_100),
                        ..Default::default()
                    })),
                ];
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("c.jsonl");
        let mut history = long_history();
        let result = compact_context(&LateUsage, &mut history, None, &archive, None).await.expect("a shaped summary");
        assert_eq!((result.input_tokens, result.output_tokens), (90_000, 5_100));
    }

    /// A prompt and `steps` tool-using steps: assistant call, then its result.
    fn running_turn(steps: usize) -> Vec<ChatMessage> {
        let mut history = vec![ChatMessage::system("SYSTEM PROMPT"), ChatMessage::user("fix the parser")];
        for i in 0..steps {
            let mut call = ChatMessage::assistant(format!("step {i} {}", "work ".repeat(400)));
            call.tool_calls = vec![flashagent_llm::ToolCall { id: format!("c{i}"), name: "read_file".into(), args_json: "{}".into() }];
            history.push(call);
            history.push(ChatMessage::tool_result(format!("c{i}"), format!("body {i} {}", "x".repeat(3000))));
        }
        history
    }

    #[test]
    fn a_running_turn_is_cut_at_a_step_and_only_when_it_has_enough_behind_it() {
        // Three steps are kept whole; what is before them must be worth a summary.
        assert_eq!(mid_turn_cut(&running_turn(3)), None, "nothing before the kept steps");
        assert_eq!(mid_turn_cut(&running_turn(4)), None, "one step behind, five thousand characters");
        assert!(mid_turn_cut(&running_turn(5)).is_some(), "two steps behind hold enough");
        let history = running_turn(8);
        let (prompt, cut) = mid_turn_cut(&history).expect("eight steps have a middle");
        assert_eq!(prompt, 1);
        assert_eq!(history[cut].role, flashagent_llm::Role::Assistant, "a tool result may not start what is kept");
        assert_eq!(history[cut..].iter().filter(|m| m.role == flashagent_llm::Role::Assistant).count(), 3);
        // Every kept result still follows its call.
        assert!(history[cut..].windows(2).all(|w| w[1].role != flashagent_llm::Role::Tool || w[0].tool_calls.iter().any(|c| Some(&c.id) == w[1].tool_call_id.as_ref())));
        // A steer or a picture inside the turn is not its prompt.
        assert_eq!(history.iter().rposition(flashagent_core::is_prompt), Some(1));
    }

    #[tokio::test]
    async fn compacting_a_running_turn_keeps_its_prompt_last_steps_and_system_memory() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("conversation.jsonl");
        let memory = "PROJECT RULES: use tabs";
        let mut history = running_turn(8);
        history[0].content = format!("SYSTEM PROMPT\n\n{memory}");
        let before = history.clone();
        let (_, cut) = mid_turn_cut(&history).unwrap();
        let saved = compact_running_turn(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, &archive, None).await;
        assert!(saved.is_some_and(|r| r.saved > 0));
        assert_eq!(history[0].role, flashagent_llm::Role::System);
        assert!(history[0].content.contains("Compacted Conversation History"));
        // Memory stays where it belongs, in the system prompt; the task itself
        // is not padded with a second copy.
        assert!(history[0].content.contains(memory), "the system prompt lost its memory:\n{}", history[0].content);
        assert_eq!(history[1].role, flashagent_llm::Role::User);
        assert_eq!(history[1].content, "fix the parser");
        // The last steps, whole and in order.
        assert_eq!(history.len(), 2 + before.len() - cut);
        assert_eq!(history[2].content, before[cut].content);
        assert_eq!(history.last().unwrap().tool_call_id, before.last().unwrap().tool_call_id);
        // What went is kept whole for the model to read.
        let archived = std::fs::read_to_string(&archive).unwrap();
        assert!(archived.contains("body 0") && !archived.contains(&format!("body {}", before.len() / 2 + 3)));

        // Nothing to cut: untouched.
        let mut short = running_turn(2);
        assert_eq!(compact_running_turn(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut short, &archive, None).await, None);
        assert_eq!(short.len(), 6);
    }

    /// The mid-turn compactor hands its result to the app through the shared
    /// slot: the core event says only how much was saved, and the card needs
    /// the summary and the call's token counts too.
    #[tokio::test]
    async fn the_mid_turn_compactor_hands_its_result_to_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("conversation.jsonl");
        let report = Arc::new(std::sync::Mutex::new(None));
        let compactor = TurnCompactor {
            source: Arc::new(ScriptedCompaction(flashagent_llm::FinishReason::Stop)),
            archive: Some(archive),
            capacity: 40_000,
            fixed: 0,
            threshold_pct: 50,
            report: report.clone(),
            sink: None,
        };
        let mut history = running_turn(8);
        let prompt_tokens = Some(30_000);
        assert!(compactor.will_compact(&history, prompt_tokens), "the window is past the threshold");
        let done = compactor.compact(&mut history, prompt_tokens).await;
        assert!(matches!(done, flashagent_core::Compaction::Saved(_)));
        let slot = report.lock().unwrap().take().expect("the app gets the result");
        assert!(slot.saved > 0);
        assert!(slot.summary.contains("## Objective"), "{}", slot.summary);
        assert!(slot.input_tokens > 0 && slot.output_tokens > 0);

        // A compaction that does not happen clears the slot first: the app must
        // never draw a stale card for a compaction that was not taken.
        assert!(!compactor.will_compact(&history, Some(1_000)), "the window has room again");
        let done = compactor.compact(&mut history, Some(1_000)).await;
        assert!(matches!(done, flashagent_core::Compaction::NotNeeded));
        assert!(report.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn single_turn_history_is_already_compact() {
        let mut history = vec![ChatMessage::system("s"), ChatMessage::user("u"), ChatMessage::assistant("a")];
        assert_eq!(compact_context(&offline_source(), &mut history, None, std::path::Path::new("unused.jsonl"), None).await, None);
        assert_eq!(history.len(), 3);
    }

    /// A `/compact` the person asked for is refused when the summary comes back
    /// truncated: a reader would take it for the whole conversation, and they
    /// asked for a summary they can trust.
    #[tokio::test]
    async fn failed_or_truncated_compaction_keeps_the_original_history() {
        let dir = tempfile::tempdir().unwrap();
        let original = vec![
            ChatMessage::system("system"),
            ChatMessage::user("important instruction"),
            ChatMessage::assistant("work already done".repeat(1000)),
            ChatMessage::user("current task"),
        ];
        // A summary that was cut off is kept, with a note about what is
        // missing: on a free model the reasoning often eats the output, and a
        // window that stays full helps nobody.
        let archive = dir.path().join("cut.jsonl");
        let mut history = original.clone();
        let cut = compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::Length), &mut history, None, &archive, None)
            .await
            .expect("what the model wrote before the cut is kept");
        assert!(cut.summary.contains("cut off at the output limit"), "and it says what it is: {}", cut.summary);
        assert!(cut.summary.contains("## Objective"), "with what it managed to write: {}", cut.summary);

        // An answer that is not a summary at all is refused, and the history
        // stays as it was.
        let archive = dir.path().join("tool.jsonl");
        let mut history = original.clone();
        assert_eq!(compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::ToolUse), &mut history, None, &archive, None).await, None);
        assert_eq!(history.len(), original.len());
        assert_eq!(history[2].content, original[2].content);

        let archive = dir.path().join("offline.jsonl");
        let mut history = original.clone();
        assert_eq!(compact_context(&offline_source(), &mut history, None, &archive, None).await, None);
        assert_eq!(history[1].content, "important instruction");
        assert!(!archive.exists());

        let blocker = dir.path().join("not_a_directory");
        std::fs::write(&blocker, "file").unwrap();
        let mut history = original.clone();
        assert_eq!(compact_context(&ScriptedCompaction(flashagent_llm::FinishReason::Stop), &mut history, None, &blocker.join("archive.jsonl"), None).await, None);
        assert_eq!(history[2].content, original[2].content, "archive failure must not discard history");
    }

    /// A window that filled up mid-task is the other way round: refusing the
    /// compaction leaves the model one step from the end of the window, so a
    /// summary that was cut off is still better than none — as long as it says
    /// out loud that it was.
    #[tokio::test]
    async fn a_window_forced_compaction_keeps_a_cut_summary_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("conversation.jsonl");
        let before = running_turn(6);
        let mut history = before.clone();
        let saved = compact_running_turn(
            &ScriptedCompaction(flashagent_llm::FinishReason::Length),
            &mut history,
            &archive,
            None,
        )
        .await;
        assert!(saved.is_some(), "a cut summary still frees the window");
        let summary = &history[0].content;
        assert!(summary.contains("cut off at the output limit"), "and says what it is: {summary}");
        assert!(summary.contains("## Objective"), "keeping what it reached: {summary}");
        assert!(history.len() < before.len(), "the window must actually be smaller: {} vs {}", history.len(), before.len());
    }

    /// The conversation must arrive exactly as the model saw it during the turn,
    /// with the instruction after it. Anything reshaped here — a new system
    /// prompt, a flattened transcript — shares no prefix with the cache and the
    /// whole conversation is prefilled again, which is what cost 2.5 minutes
    /// at 80k tokens.
    #[test]
    fn the_compaction_request_keeps_the_conversation_as_a_cached_prefix() {
        let mut call = ChatMessage::assistant("");
        call.tool_calls.push(flashagent_llm::ToolCall {
            id: "c1".into(), name: "run_shell".into(), args_json: r#"{"command":"cargo test"}"#.into(),
        });
        let conversation = vec![
            ChatMessage::system("You are FlashAgent, in ~/work.\n\n[memory]"),
            ChatMessage::user("memory preamble\n\n---\n\n[AUTONOMOUS GOAL DIRECTIVE]\nTarget goal: ship the release\nBudget: 8 steps"),
            call,
            ChatMessage::tool_result("c1", "tests passed"),
            ChatMessage::assistant("Tests pass. Next: check the release notes."),
        ];
        let request = compaction_request(&conversation, None, None, Detail::Brief, false);

        // The first four messages are the conversation, byte for byte: that is
        // the whole point, so the server's cache of it still applies.
        for (i, (sent, was)) in request.iter().zip(&conversation).enumerate() {
            assert_eq!(sent.role, was.role, "message {i} changed role");
            assert_eq!(sent.content, was.content, "message {i} changed content");
            assert_eq!(sent.tool_call_id, was.tool_call_id, "message {i} changed its call link");
            assert_eq!(sent.tool_calls.len(), was.tool_calls.len(), "message {i} lost a tool call");
        }
        assert_eq!(request.last().unwrap().role, flashagent_llm::Role::User, "the instruction is the last message");
        let instruction = &request.last().unwrap().content;
        assert!(!instruction.contains("Target goal"), "the instruction stands on its own, not wrapped in the user's words");
        assert!(instruction.contains("Summarize the conversation above"), "{instruction}");
        // No second system prompt to throw the prefix away.
        assert_eq!(request.iter().filter(|m| m.role == flashagent_llm::Role::System).count(), 1);
    }

    /// A user message may not follow a tool result; providers that check the
    /// pairing reject the request outright. The dropped tail is summarized.
    #[test]
    fn a_dangling_tool_result_is_dropped_before_the_instruction() {
        let conversation = vec![
            ChatMessage::user("do the thing"),
            ChatMessage::tool_result("c1", "out"),
            ChatMessage::tool_result("c2", "out"),
        ];
        let request = compaction_request(&conversation, None, None, Detail::Brief, false);
        assert!(!request.iter().any(|m| m.role == flashagent_llm::Role::Tool), "a tool result was left hanging before a user message");
        assert_eq!(request.last().unwrap().role, flashagent_llm::Role::User);
    }

    /// What the user asked to be kept survives into the instruction.
    #[test]
    fn focus_instructions_reach_the_summariser() {
        let request = compaction_request(&[ChatMessage::user("hi")], None, Some("keep the migration notes"), Detail::Brief, false);
        assert!(request.last().unwrap().content.contains("keep the migration notes"));
    }

    #[test]
    fn compaction_summary_needs_every_card_section() {
        assert!(!summary_has_handoff_state("## Objective\ndo the work"));
        assert!(!summary_has_handoff_state("## Objective\ndo the work\n## Requirements\n- none\n## Work State\n### Completed\n- none"));
        // Plain headings are accepted: normalization rewrites them before this
        // check, but has_heading itself must not care about punctuation.
        assert!(summary_has_handoff_state(
            "Objective:\ndo the work\n\nRequirements:\n- none\n\nDecisions:\n- none\n\nWork State:\nCompleted:\n- nothing\nActive:\n- nothing\nBlocked:\n- nothing\n\nNext Move:\n1. wait\n\nRelevant Files:\n- none\n\nImportant Context:\n- none"
        ));
        assert!(!summary_has_handoff_state("Summary:\n1. Primary Request and Intent: do the work\n7. Pending Tasks: None\n8. Current Work: waiting"));
    }

    /// A window that filled up mid-task has to hand the model everything it
    /// needs to finish, because the summary is its only memory of the work
    /// already done.
    #[test]
    fn a_window_forced_summary_asks_for_everything_and_the_card_shape() {
        let request = compaction_request(&[ChatMessage::user("do it")], None, None, Detail::Exhaustive, false);
        let instruction = &request.last().unwrap().content;
        assert!(instruction.to_lowercase().contains("keep everything"), "{instruction}");
        assert!(instruction.contains("Length is not the problem"), "it must not be told to be brief: {instruction}");
        assert!(instruction.contains("line numbers"), "detail is spelled out, not just requested: {instruction}");
        assert!(instruction.contains("verbatim"), "errors are kept as they were: {instruction}");
        // The card's shape is stated in the order it is drawn, so a summary cut
        // off at the output limit still says what was done and what is next.
        let objective_at = instruction.find("## Objective").expect("no Objective block");
        let work_at = instruction.find("## Work State").expect("no Work State block");
        let next_at = instruction.find("## Next Move").expect("no Next Move block");
        assert!(objective_at < work_at && work_at < next_at);
        assert!(!instruction.contains("Drop repeated output and dead ends"), "brevity must not be asked for here");
    }

    /// A `/compact` the person asked for is a different thing: they are reading
    /// along, and the archive is the way back to the rest.
    #[test]
    fn an_asked_for_summary_stays_short_and_points_at_the_archive() {
        let request = compaction_request(&[ChatMessage::user("compact")], None, None, Detail::Brief, false);
        let instruction = &request.last().unwrap().content;
        assert!(instruction.contains("exact transcript stays in the archive"), "{instruction}");
        assert!(!instruction.contains("Length is not the problem"), "brevity is not forbidden for a summary a person asked for");
    }

    /// The detail a caller asks for is the detail that is requested, whichever
    /// way round the two are passed.
    #[test]
    fn the_detail_asked_for_is_the_detail_requested() {
        for (detail, keeps_everything) in [(Detail::Exhaustive, true), (Detail::Brief, false)] {
            let request = compaction_request(&[ChatMessage::user("x")], None, None, detail, false);
            let instruction = &request.last().unwrap().content;
            assert_eq!(instruction.contains("Length is not the problem"), keeps_everything, "{detail:?}");
        }
    }

    /// The shape the card and the model both expect.
    #[test]
    fn every_summary_the_card_accepts_has_the_same_headings() {
        assert!(summary_has_handoff_state(SHAPED_SUMMARY));
        assert!(!summary_has_handoff_state("Objective:\nonly the goal, no work state"));
    }

    /// The retry is the last thing between a full window and a lost summary, so
    /// it spells the shape out one heading per line: the previous version ran
    /// them together, and a model that copied it wrote `Work State (with ...)`
    /// as a heading, which is not the heading the validator accepts.
    #[test]
    fn the_retry_lists_every_heading_on_its_own_line() {
        let request = compaction_request(&[ChatMessage::user("do it")], None, None, Detail::Exhaustive, true);
        let instruction = &request.last().unwrap().content;
        assert!(instruction.contains("## Work State\n### Completed\n### Active\n### Blocked\n## Next Move"), "{instruction}");
        assert!(!instruction.contains("Work State (with"), "{instruction}");
    }

    #[test]
    fn steering_injected_event_unpins_pending_steer_and_adds_user_message() {
        let mut chat = ChatView::default();
        chat.push_user("first prompt");
        let mut pending_steers: Vec<(String, Vec<Attachment>)> = vec![("please use postgres".to_string(), Vec::new())];

        let ev = LoopEvent::SteeringInjected("please use postgres".to_string());
        if let LoopEvent::SteeringInjected(ref directive) = ev {
            if let Some(pos) = pending_steers.iter().position(|(text, _)| text == directive) {
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
    fn a_preset_the_code_never_heard_of_still_reaches_the_request_verbatim() {
        let cfg = AppConfig::default();
        // Models invent levels: `xhigh`, `max`, `ultra`. The app must not need
        // to know a name to be able to send it, or such levels are unusable.
        for preset in ["xhigh", "max", "ultra", "gpt-5-thinking-high", "VeryLow"] {
            let opts = build_turn_options(&cfg, preset);
            assert_eq!(opts.custom_effort.as_deref(), Some(preset), "{preset} was rewritten or dropped");
            // Unknown names keep the typed default so no fixed mapping guesses
            // over the top of what the user asked for.
            assert_eq!(opts.thinking, flashagent_llm::ThinkingEffort::Default, "{preset}");
        }
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
        let view = settings_for_runtime(&persisted, PermissionMode::Bypass, "high", "m", &[], 131_072, flashagent_tui::settings::effort_choices_defaults());
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
        let view = settings_for_runtime(&cfg, PermissionMode::Bypass, "high", "gemma", &[], 131_072, flashagent_tui::settings::effort_choices_defaults());
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
            reachable_without_listing: false,
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


