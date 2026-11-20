//! Built-in toolset implementing [`flashagent_core::ToolExec`] over a fixed
//! working directory.

pub mod ask_user;
pub mod env_tools;
pub mod fs_tools;
pub mod git;
pub mod mcp;
pub mod image_tool;
pub mod memory_tools;
pub mod outline;
pub mod patch;
pub mod plan_tool;
pub mod shell;
pub mod skill_tools;
pub mod subagents;
pub mod web;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flashagent_core::{ToolExec, ToolOutput, WritePreview};
use flashagent_llm::{ToolCall, ToolSpec};
use serde::de::DeserializeOwned;
use serde::Deserialize;
pub use shell::{ShellRegistry, TaskInfo, TaskNotice, TaskState};

pub use ask_user::{QuestionGate, GOAL_QUESTION_TIMEOUT};
pub use subagents::{agent_tools, BuiltinSubagentFactory, CompositeTools, ToolSubset};

/// Hard cap on a tool result fed back to the model.
const MAX_OUTPUT_CHARS: usize = 32_000;
/// A byte budget before the final character cap, even for non-ASCII output.
const MAX_OUTPUT_BYTES: usize = MAX_OUTPUT_CHARS * 4;
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// Append without first allocating a whole long line or result chunk.
fn push_capped(out: &mut String, text: &str, max: usize) -> bool {
    let mut end = text.len().min(max.saturating_sub(out.len()));
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    out.push_str(&text[..end]);
    end == text.len()
}

/// Surfaced as tool results (`is_error`), never as panics.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Bad args, timeouts, HTTP failures.
    #[error("{0}")]
    Other(String),
}

/// Read exactly as the permission layer and approval card read them
/// (`effective_args`), with no fallback to another part of the payload.
fn parse_args<T: DeserializeOwned>(json: &str, tool: &str) -> Result<T, ToolError> {
    let value = flashagent_llm::effective_args(json, tool)
        .ok_or_else(|| ToolError::Other(format!("bad arguments: not a JSON object: {json}")))?;
    serde_json::from_value(value).map_err(|e| ToolError::Other(format!("bad arguments: {e}")))
}

/// How long one foreground command may hold a turn. A turn waiting on a shell
/// cannot steer, cannot be interrupted and cannot answer; anything longer than
/// this is a background task, which returns at once and reports when it exits.
pub const MAX_FOREGROUND_TIMEOUT_MS: u64 = 600_000;

/// Cut in place, looking no further than the cut: a diff or a log can be
/// megabytes.
fn truncate_output(mut text: String) -> String {
    if let Some((cut, _)) = text.char_indices().nth(MAX_OUTPUT_CHARS) {
        text.truncate(cut);
        text.push_str(&format!("\n...[truncated at {MAX_OUTPUT_CHARS} chars]"));
    }
    text
}

/// Where a `chat_reply` goes: the host (serve) turns one call into one chat
/// message the bot relays as a reply to that member. The TUI has none.
pub type ReplyOutbox = std::sync::Arc<dyn Fn(i64, &str) + Send + Sync>;

#[derive(Default)]
pub struct BuiltinToolsConfig {
    /// For all filesystem tools and the shell.
    pub cwd: PathBuf,
    /// Without it `web_search` uses DuckDuckGo.
    pub brave_api_key: Option<String>,
    pub question_gate: Option<Arc<dyn QuestionGate>>,
    /// Set while `/goal` runs.
    pub is_goal_mode: Option<Arc<AtomicBool>>,
    pub web_enabled: Option<bool>,
    /// Kept for source compatibility; schema budgets no longer depend on it.
    pub context_window: Option<usize>,
    pub mcp_manager: Option<Arc<mcp::McpManager>>,
    /// Tools the owner has switched off. They are not offered, and a call to
    /// one is refused.
    pub disabled_tools: Option<Vec<String>>,
    /// Where `~` and the memory stores point. `None` reads HOME; a test sets
    /// it so no memory is written into the owner's home directory.
    pub home: Option<PathBuf>,
    /// The chat outbox: set by `--serve`, whose bot relays each call to the
    /// room as a reply to one member's message. `None` leaves `chat_reply`
    /// off the toolset entirely — a terminal session has no chat to talk to.
    pub reply_outbox: Option<ReplyOutbox>,
}

/// What a tool answered: text for the model, or a picture.
///
/// `view_image` is the one tool whose result is not text, so the dispatcher
/// answers with one or the other instead of pretending every tool is the same.
enum Answer {
    Text(String),
    Picture(ToolOutput),
}

/// One edit plan supplies both the approval diff and the eventual writes.
struct PreparedEdit {
    files: Result<Vec<fs_tools::PlannedFile>, ToolError>,
    diff: Option<String>,
    single: bool,
}

impl PreparedEdit {
    fn commit(self) -> Result<String, ToolError> {
        self.files.and_then(|files| {
            let summary = fs_tools::write_planned(&files)?;
            Ok(if self.single {
                format!("applied {} edit(s) to {}", files[0].replacements, files[0].path)
            } else {
                summary
            })
        })
    }
}

#[async_trait]
impl flashagent_core::loop_::PreparedWrite for PreparedEdit {
    fn preview(&self) -> Option<String> {
        self.diff.clone()
    }

    fn error(&self) -> Option<String> {
        self.files.as_ref().err().map(|e| truncate_output(format!("error: {e}")))
    }

    async fn execute(self: Box<Self>) -> ToolOutput {
        text_output((*self).commit())
    }
}

fn text_output(result: Result<String, ToolError>) -> ToolOutput {
    match result {
        Ok(text) => ToolOutput { content: truncate_output(text), is_error: false, images: Vec::new() },
        Err(e) => ToolOutput { content: truncate_output(format!("error: {e}")), is_error: true, images: Vec::new() },
    }
}

/// Everything the offered tool list depends on.
///
/// Two calls with the same key offer the same tools, so the list is measured
/// once and kept until one of these changes.
#[derive(PartialEq, Eq)]
struct SpecsKey {
    prompt_tokens: usize,
    model: String,
    web: bool,
    vision: bool,
    disabled: Vec<String>,
}

/// The measured tool list, and what it was measured for.
struct SpecsCache {
    key: SpecsKey,
    specs: Vec<ToolSpec>,
}

/// One per session.
pub struct BuiltinTools {
    cwd: PathBuf,
    /// `None` means HOME, read when a memory call needs it.
    home: Option<PathBuf>,
    shells: ShellRegistry,
    http: reqwest::Client,
    brave_key: Option<String>,
    question_gate: Option<Arc<dyn QuestionGate>>,
    is_goal_mode: Arc<AtomicBool>,
    web_enabled: Arc<AtomicBool>,
    /// What the system prompt actually costs, in tokens. The schemas are given
    /// what a tier leaves once this has taken its share, and the prompt is not
    /// something a tier may shrink, so this number decides how many schemas
    /// there is room for. Defaults to the ceiling, which is the safe direction:
    /// if the prompt ever really fills it, a small window still gets tools.
    prompt_tokens: Arc<std::sync::atomic::AtomicUsize>,
    /// Which model the schemas are going to, because the count is that model's.
    model_name: Arc<std::sync::RwLock<String>>,
    /// Set from what the server said about the model.
    vision_supported: Arc<std::sync::atomic::AtomicBool>,
    mcp_manager: Arc<mcp::McpManager>,
    /// The tools that are off. Shared with the app, so the switch takes effect
    /// in the conversation that is running.
    disabled: Arc<std::sync::RwLock<Vec<String>>>,
    /// The chat outbox behind `chat_reply`; `None` keeps the tool unoffered.
    reply_outbox: Option<ReplyOutbox>,
    /// The measured tool list. Measuring it means tokenizing every schema,
    /// and the list is asked for on every tool call.
    specs_cache: parking_lot::Mutex<Option<SpecsCache>>,
    /// How often the list was measured: what the cache is tested with.
    #[cfg(test)]
    specs_measured: std::sync::atomic::AtomicUsize,
}

impl BuiltinTools {
    pub fn new(config: BuiltinToolsConfig) -> Result<Self, ToolError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| ToolError::Other(format!("http client: {e}")))?;
        let mcp_manager = config
            .mcp_manager
            .unwrap_or_else(|| mcp::McpManager::new(config.cwd.clone()));
        Ok(Self {
            cwd: config.cwd.clone(),
            home: config.home,
            shells: ShellRegistry::with_cwd(config.cwd),
            http,
            brave_key: config.brave_api_key,
            question_gate: config.question_gate,
            is_goal_mode: config.is_goal_mode.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
            web_enabled: Arc::new(AtomicBool::new(config.web_enabled.unwrap_or(true))),
            prompt_tokens: Arc::new(std::sync::atomic::AtomicUsize::new(
                flashagent_core::budget::PROMPT_CEILING,
            )),
            model_name: Arc::new(std::sync::RwLock::new(String::new())),
            vision_supported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            mcp_manager,
            disabled: Arc::new(std::sync::RwLock::new(config.disabled_tools.unwrap_or_default())),
            reply_outbox: config.reply_outbox,
            specs_cache: parking_lot::Mutex::new(None),
            #[cfg(test)]
            specs_measured: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    /// Background commands, and the foreground one the user can move there.
    pub fn shells(&self) -> &ShellRegistry {
        &self.shells
    }

    pub fn mcp_manager(&self) -> Arc<mcp::McpManager> {
        self.mcp_manager.clone()
    }

    pub fn set_goal_mode(&self, active: bool) {
        self.is_goal_mode.store(active, Ordering::Relaxed);
    }

    /// The tools that are off. The app changes them while the session runs.
    pub fn set_disabled_tools(&self, names: Vec<String>) {
        if let Ok(mut off) = self.disabled.write() {
            *off = names;
        }
    }

    /// The tools that are off right now.
    pub fn disabled_tools(&self) -> Vec<String> {
        self.disabled.read().map(|d| d.clone()).unwrap_or_default()
    }

    /// Whether this tool is one of the ones that are off.
    pub fn tool_disabled(&self, name: &str) -> bool {
        self.disabled.read().map(|d| d.iter().any(|n| n == name)).unwrap_or(false)
    }

    pub fn set_web_enabled(&self, enabled: bool) {
        self.web_enabled.store(enabled, Ordering::Relaxed);
    }

    /// `view_image` is only offered, and only works, when the model can see.
    pub fn set_vision_supported(&self, supported: bool) {
        self.vision_supported.store(supported, Ordering::Relaxed);
    }

    pub fn vision_supported(&self) -> bool {
        self.vision_supported.load(Ordering::Relaxed)
    }

    /// Compatibility setter for callers from before schema budgets became flat.
    pub fn set_context_window(&self, _window: Option<usize>) {}

    /// What the system prompt measured this session, in tokens.
    pub fn set_prompt_tokens(&self, tokens: usize) {
        self.prompt_tokens.store(tokens, Ordering::Relaxed);
    }

    /// The model the schemas are counted for. A budget measured with another
    /// model's tokenizer is another model's budget.
    pub fn set_model_name(&self, name: &str) {
        if let Ok(mut lock) = self.model_name.write() {
            *lock = name.to_string();
        }
    }

    /// Relative to the project when inside it; models often return absolute paths.
    fn shown_path(&self, path: &str) -> String {
        std::path::Path::new(path)
            .strip_prefix(&self.cwd)
            .map(|rel| rel.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string())
    }

    /// Everything the offered list depends on.
    fn specs_key(&self) -> SpecsKey {
        SpecsKey {
            prompt_tokens: self.prompt_tokens.load(Ordering::Relaxed),
            model: self.model_name.read().ok().map(|m| m.clone()).unwrap_or_default(),
            web: self.web_enabled.load(Ordering::Relaxed),
            vision: self.vision_supported.load(Ordering::Relaxed),
            disabled: self.disabled_tools(),
        }
    }

    /// How often the list was measured from scratch. What the cache is tested
    /// with.
    #[cfg(test)]
    pub(crate) fn specs_measured(&self) -> usize {
        self.specs_measured.load(Ordering::Relaxed)
    }

    /// The tool list, measured. Every schema is tokenized with the model it
    /// is going to, which is the expensive part.
    fn measured_specs(&self) -> Vec<ToolSpec> {
        use flashagent_core::registry::{self, Handler};

        // One toolset for every model: the budget trims it if it must, the
        // owner switches tools off by hand.
        let core = true;
        let web = self.web_enabled.load(Ordering::Relaxed);
        let vision = self.vision_supported();

        // One list, read from the registry: a tool's name, its schema and what it
        // is for cannot disagree with each other any more.
        let mut specs: Vec<ToolSpec> = registry::TOOLS
            .iter()
            // The three that only make sense at the top, added by the composite
            // in `subagents.rs`: offering them here too would put two tools with
            // one name in front of the model.
            .filter(|def| {
                !matches!(
                    def.handler,
                    Handler::Agent | Handler::SpawnAgent | Handler::SendMessage | Handler::ReviewAgent
                )
            })
            // `BuiltinTools` is what a subagent is given, so the parent-only
            // tools are not offered from here. The chat outbox makes
            // `chat_reply` real; without it the tool is not on the table.
            .filter(|def| registry::offered(def, core, vision, web, false, self.reply_outbox.is_some()))
            // A tool the owner has switched off is not on the table at all.
            .filter(|def| !self.tool_disabled(def.name))
            .map(|def| ToolSpec {
                name: def.name.to_string(),
                description: def.description.to_string(),
                parameters_json: def.schema_json(),
            })
            .collect();

        // Required tools stay; optional schemas fit the allowance left by the
        // measured prompt and MCP share.
        {
        let budget = flashagent_core::budget::Budget::for_window(None);
        let model_name = self.model_name.read().ok().map(|m| m.clone()).unwrap_or_default();
        let allowance = budget.tool_allowance(self.prompt_tokens.load(Ordering::Relaxed));
        let measured: Vec<(&str, bool, usize)> = specs
            .iter()
            .map(|spec| {
                (
                    spec.name.as_str(),
                    registry::TOOLS
                        .iter()
                        .find(|def| def.name == spec.name)
                        .is_some_and(|def| matches!(def.availability, flashagent_core::registry::Availability::Always)),
                    flashagent_core::budget::tool_cost(
                        &model_name,
                        &spec.name,
                        &spec.description,
                        &spec.parameters_json,
                    ),
                )
            })
            .collect();
        let (kept, _dropped) = flashagent_core::budget::fit_builtin(measured.into_iter(), allowance);
        let kept: std::collections::HashSet<String> = kept.into_iter().map(str::to_string).collect();
        specs.retain(|spec| kept.contains(&spec.name));
        }
        #[cfg(test)]
        self.specs_measured.fetch_add(1, Ordering::Relaxed);
        specs
    }

    fn prepare_edit(&self, call: &ToolCall) -> PreparedEdit {
        let args = parse_args::<EditToolArgs>(&call.args_json, &call.name);
        let single = args.as_ref().is_ok_and(|a| a.files.is_empty());
        let files = args.and_then(|args| {
            if args.files.is_empty() {
                if args.path.trim().is_empty() || args.edits.is_empty() {
                    return Err(ToolError::Other("edit mode replace needs a path and edits, or files to edit several".into()));
                }
                fs_tools::plan_edit(&self.cwd, &args.path, &args.edits)
            } else {
                fs_tools::plan_edits(&self.cwd, &args.targets())
            }
        });
        let diff = files.as_ref().ok().map(|files| {
            files.iter().map(|file| {
                flashagent_core::unified(Some(&file.original), &file.new, &self.shown_path(&file.path), 3)
            }).collect::<Vec<_>>().join("\n")
        });
        PreparedEdit { files, diff, single }
    }

    /// `None` when the call is not a write or the file cannot be read. Which
    /// diff a call needs is declared next to its schema, so a new tool that
    /// changes files is shown on the card without anyone remembering this.
    fn preview_for(&self, call: &ToolCall) -> Option<String> {
        use flashagent_core::registry::Preview;
        let v = flashagent_llm::effective_args(&call.args_json, &call.name)?;
        match flashagent_core::registry::find(&call.name)?.preview {
            Preview::Write => {
                let path = v.get("path")?.as_str()?;
                let new = v.get("content")?.as_str()?;
                let old = fs_tools::read_raw(&self.cwd, path);
                Some(flashagent_core::unified(old.as_deref(), new, &self.shown_path(path), 3))
            }
            Preview::Edit => self.prepare_edit(call).diff,
            Preview::Patch => {
                let path = v.get("path")?.as_str()?;
                let patch_text = v.get("patch")?.as_str()?;
                patch::preview_patch(&self.cwd, path, patch_text)
            }
            Preview::File => {
                let mode = v.get("mode")?.as_str()?.trim().to_ascii_lowercase();
                match mode.as_str() {
                    "write" => {
                        let path = v.get("path")?.as_str()?;
                        let new = v.get("content")?.as_str()?;
                        let old = fs_tools::read_raw(&self.cwd, path);
                        Some(flashagent_core::unified(old.as_deref(), new, &self.shown_path(path), 3))
                    }
                    "replace" => self.prepare_edit(call).diff,
                    "patch" => {
                        let path = v.get("path")?.as_str()?;
                        let patch_text = v.get("patch")?.as_str()?;
                        patch::preview_patch(&self.cwd, path, patch_text)
                    }
                    _ => None,
                }
            }
            Preview::None => None,
        }
    }

    /// Reads that can walk a whole tree, read a big file or wait on a process
    /// run on the blocking pool: the async workers stay free, and the loop
    /// still sees a cancel while one runs. Writes stay inline, so a cancelled
    /// turn never reports a change as not made while it lands anyway.
    async fn off_runtime<T: Send + 'static>(
        &self,
        work: impl FnOnce(&std::path::Path) -> Result<T, ToolError> + Send + 'static,
    ) -> Result<T, ToolError> {
        let cwd = self.cwd.clone();
        tokio::task::spawn_blocking(move || work(&cwd))
            .await
            .map_err(|e| ToolError::Other(format!("the tool stopped unexpectedly: {e}")))?
    }

    async fn dispatch(&self, call: &ToolCall) -> Result<Answer, ToolError> {
        use flashagent_core::registry::Handler;
        // A built-in tool runs the handler the registry named for it. The match
        // is on the handler, not on the name, so a tool declared with no code
        // anywhere is a compile error rather than a call that answers
        // "unknown tool" a user has to report.
        // A prompt from before the rename taught `update_plan`; a model that
        // kept the habit answers through the new tool.
        let name = flashagent_core::registry::canonical_name(&call.name);
        let Some(def) = flashagent_core::registry::find(name) else {
            if call.name.starts_with("mcp__") || self.mcp_manager.has_tool(&call.name) {
                return self.mcp_manager.call_tool(&call.name, &call.args_json).await.map(Answer::Text);
            }
            return Err(ToolError::Other(format!("unknown tool: {}", call.name)));
        };
        match def.handler {
            Handler::Read => {
                let a: ReadArgs = parse_args(&call.args_json, &call.name)?;
                let mode = a.mode.as_deref().unwrap_or("auto").trim().to_ascii_lowercase();
                let looks_like_image = std::path::Path::new(&a.path)
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"));
                if mode == "image" || (mode == "auto" && looks_like_image && a.files.is_empty()) {
                    if a.path.trim().is_empty() {
                        return Err(ToolError::Other("read: mode image needs one path".into()));
                    }
                    let args = serde_json::json!({ "path": a.path }).to_string();
                    let vision = self.vision_supported();
                    let opened = self.off_runtime(move |cwd| Ok(image_tool::view_image(cwd, &args, vision))).await;
                    Ok(Answer::Picture(opened.unwrap_or_else(|e| ToolOutput {
                        content: format!("error: {e}"),
                        is_error: true,
                        images: Vec::new(),
                    })))
                } else {
                    Ok(Answer::Text(self.off_runtime(move |cwd| a.read(cwd)).await?))
                }
            }
            Handler::Search => {
                let a: SearchToolArgs = parse_args(&call.args_json, &call.name)?;
                a.run(self).await.map(Answer::Text)
            }
            Handler::Edit => {
                let a: EditToolArgs = parse_args(&call.args_json, &call.name)?;
                match a.mode.trim().to_ascii_lowercase().as_str() {
                    "write" => {
                        let content = a.content.ok_or_else(|| ToolError::Other("edit mode write needs content".into()))?;
                        if a.path.trim().is_empty() {
                            return Err(ToolError::Other("edit mode write needs a path".into()));
                        }
                        fs_tools::write_file(&self.cwd, &a.path, &content).map(Answer::Text)
                    }
                    "replace" => self.prepare_edit(call).commit().map(Answer::Text),
                    "patch" => {
                        let patch_text = a.patch.ok_or_else(|| ToolError::Other("edit mode patch needs a patch".into()))?;
                        if a.path.trim().is_empty() {
                            return Err(ToolError::Other("edit mode patch needs a path".into()));
                        }
                        patch::patch_file(&self.cwd, &a.path, &patch_text).map(Answer::Text)
                    }
                    other => Err(ToolError::Other(format!(
                        "edit: unknown mode '{other}'; use write, replace or patch"
                    ))),
                }
            }
            Handler::Git => {
                let a: GitArgs = parse_args(&call.args_json, &call.name)?;
                let action = a.action.trim().to_ascii_lowercase();
                match action.as_str() {
                    "status" => git::git_status(&self.cwd, a.path.as_deref()).await.map(Answer::Text),
                    "diff" => git::git_diff(&self.cwd, a.staged.unwrap_or(false), a.path.as_deref()).await.map(Answer::Text),
                    "log" => git::git_log(&self.cwd, a.path.as_deref(), a.limit).await.map(Answer::Text),
                    "show" => git::git_show(&self.cwd, a.rev.as_deref(), a.path.as_deref()).await.map(Answer::Text),
                    "blame" => git::git_blame(&self.cwd, a.rev.as_deref(), a.path.as_deref()).await.map(Answer::Text),
                    other => Err(ToolError::Other(format!(
                        "git: unknown action '{other}'; use status, diff, log, show or blame"
                    ))),
                }
            }
            Handler::RunShell => {
                let a: ShellArgs = parse_args(&call.args_json, &call.name)?;
                self.run_shell(a).await.map(Answer::Text)
            }
            Handler::EnvInfo => env_tools::env_info(&self.cwd).await.map(Answer::Text),
            Handler::UserInteraction => {
                let a: ask_user::UserInteractionArgs = parse_args(&call.args_json, &call.name)?;
                ask_user::run_user_interaction(
                    &self.cwd,
                    self.question_gate.as_ref(),
                    &self.is_goal_mode,
                    a,
                    ask_user::GOAL_QUESTION_TIMEOUT,
                )
                .await
                .map(Answer::Text)
            }
            Handler::Plan => plan_tool::run_plan(&call.args_json).map(Answer::Text),
            Handler::ChatReply => {
                let outbox = self.reply_outbox.as_ref().ok_or_else(|| {
                    ToolError::Other("chat_reply is not available here: nothing serves a chat".into())
                })?;
                let a: ChatReplyArgs = parse_args(&call.args_json, &call.name)?;
                (outbox)(a.message_id, &a.text);
                Ok(Answer::Text(format!("Reply sent to msg:{}.", a.message_id)))
            }
            Handler::Memory => {
                let a: memory_tools::MemoryArgs = parse_args(&call.args_json, &call.name)?;
                memory_tools::memory(&self.cwd, self.home.as_deref(), &self.is_goal_mode, a).map(Answer::Text)
            }
            Handler::Web => {
                if !self.web_enabled.load(Ordering::Relaxed) {
                    return Err(ToolError::Other("web is turned off in Settings (LLM & Reasoning → Web Tools).".into()));
                }
                let a: WebArgs = parse_args(&call.args_json, &call.name)?;
                match a.action.trim().to_ascii_lowercase().as_str() {
                    "search" => {
                        let query = a.query.ok_or_else(|| ToolError::Other("web search needs a query".into()))?;
                        let count = a.count.unwrap_or(5);
                        let key = self.brave_key.as_deref().map(str::trim).filter(|k| !k.is_empty());
                        match key {
                            Some(key) => match web::search(&self.http, key, &query, count).await {
                                Ok(found) => Ok(Answer::Text(found)),
                                Err(_) => web::search_free(&self.http, &query, count).await.map(Answer::Text),
                            },
                            None => web::search_free(&self.http, &query, count).await.map(Answer::Text),
                        }
                    }
                    "fetch" => {
                        let url = a.url.ok_or_else(|| ToolError::Other("web fetch needs a url".into()))?;
                        web::fetch_text(&url).await.map(Answer::Text)
                    }
                    other => Err(ToolError::Other(format!("web: unknown action '{other}'; use search or fetch"))),
                }
            }
            Handler::HttpRequest => {
                if !self.web_enabled.load(Ordering::Relaxed) {
                    return Err(ToolError::Other("http_request is turned off in Settings (LLM & Reasoning → Web Tools).".into()));
                }
                let a: HttpRequestArgs = parse_args(&call.args_json, &call.name)?;
                let method = a.method.unwrap_or_else(|| "GET".to_string());
                let headers: Vec<(String, String)> = a.headers.into_iter().map(|h| (h.name, h.value)).collect();
                web::request_text(&method, &a.url, &headers, a.body.as_deref()).await.map(Answer::Text)
            }
            Handler::Skill => {
                let a: SkillArgs = parse_args(&call.args_json, &call.name)?;
                skill_tools::skill(&self.cwd, self.home.as_deref(), a.action.trim(), a.name.as_deref()).map(Answer::Text)
            }
            Handler::Task => {
                let a: TaskArgs = parse_args(&call.args_json, &call.name)?;
                self.run_task(a).await.map(Answer::Text)
            }
            Handler::Agent => Err(ToolError::Other(
                "agent is not available in this set: it is added by the app, one level up.".into(),
            )),
            // `spawn_agent`, `send_message` and `review_agent` have handlers here
            // so the registry covers every tool the model is ever offered, but
            // their code lives in the core crate: they need the mailbox and the
            // loop, and this is the plain toolset a child is given.
            Handler::SpawnAgent => Err(ToolError::Other(
                "spawn_agent is not available in this set: it is added by the app, one level up.".into(),
            )),
            Handler::SendMessage | Handler::ReviewAgent => Err(ToolError::Other(format!(
                "{} is not available in this set: it is added by the app, one level up.",
                def.name
            ))),
        }
    }

    async fn run_task(&self, a: TaskArgs) -> Result<String, ToolError> {
        let action = a.action.trim().to_ascii_lowercase();
        match action.as_str() {
            "list" => {
                let tasks = self.shells.tasks();
                if tasks.is_empty() {
                    return Ok("No background tasks.".to_string());
                }
                Ok(tasks
                    .into_iter()
                    .map(|task| {
                        let output = if task.last_line.is_empty() { String::new() } else { format!(" — {}", task.last_line) };
                        format!(
                            "{}: {} for {}{}{}",
                            task.id,
                            task.state.describe(),
                            shell::format_elapsed(task.elapsed),
                            if task.detached { " (moved to background by the user)" } else { "" },
                            output
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
            "output" => {
                let id = a.task_id.ok_or_else(|| ToolError::Other("task output needs task_id".into()))?;
                self.shells.status(id)
            }
            "stop" => {
                let id = a.task_id.ok_or_else(|| ToolError::Other("task stop needs task_id".into()))?;
                self.shells.kill(id).await
            }
            other => Err(ToolError::Other(format!("task: unknown action '{other}'; use list, output or stop"))),
        }
    }

    async fn run_shell(&self, a: ShellArgs) -> Result<String, ToolError> {
        let cmd = a
            .command
            .ok_or_else(|| ToolError::Other("run_shell: `command` is required; background:true starts a task, and `task` reads or stops one".into()))?;
        if a.background.unwrap_or(false) {
            self.shells.spawn_background(&cmd)
        } else {
            // A turn that waits is a turn that cannot steer, interrupt or answer.
            // Past this the job belongs in the background, and the schema says so.
            let asked = a.timeout_ms.unwrap_or(120_000);
            let timeout = Duration::from_millis(asked.min(MAX_FOREGROUND_TIMEOUT_MS));
            self.shells
                .run_foreground(&cmd, timeout)
                .await
                .map_err(|e| match e {
                    ToolError::Other(message) if asked > MAX_FOREGROUND_TIMEOUT_MS && message.contains("timeout") => {
                        ToolError::Other(format!(
                            "{message} (asked for {asked}ms, the ceiling for a foreground command is \
                             {MAX_FOREGROUND_TIMEOUT_MS}ms; run it with background:true instead)"
                        ))
                    }
                    other => other,
                })
        }
    }
}

impl WritePreview for BuiltinTools {
    fn write_preview(&self, call: &ToolCall) -> Option<String> {
        self.preview_for(call)
    }

    fn prepare(&self, call: &ToolCall) -> Option<Box<dyn flashagent_core::loop_::PreparedWrite>> {
        if call.name != "edit" {
            return None;
        }
        let args: EditToolArgs = parse_args(&call.args_json, &call.name).ok()?;
        if !args.mode.trim().eq_ignore_ascii_case("replace") {
            return None;
        }
        if args.targets().iter().any(|(path, _)| !flashagent_core::path_is_inside(&self.cwd, path)) {
            return None;
        }
        Some(Box::new(self.prepare_edit(call)))
    }

    /// A quoted near match outside the project would itself be an unapproved read.
    fn doomed(&self, call: &ToolCall) -> Option<String> {
        if call.name != "edit" {
            return None;
        }
        let args: EditToolArgs = parse_args(&call.args_json, &call.name).ok()?;
        if !args.mode.trim().eq_ignore_ascii_case("replace") {
            return None;
        }
        if args.targets().iter().any(|(path, _)| !flashagent_core::path_is_inside(&self.cwd, path)) {
            return None;
        }
        self.prepare_edit(call).files.err().map(|e| truncate_output(format!("error: {e}")))
    }
}

#[async_trait]
impl ToolExec for BuiltinTools {
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        if self.tool_disabled(&call.name) {
            return ToolOutput {
                content: format!(
                    "the tool '{}' is switched off in this session · the user turned it off, so do not call it",
                    call.name
                ),
                is_error: true,
                images: Vec::new(),
            };
        }
        match self.dispatch(call).await {
            Ok(Answer::Text(text)) => ToolOutput { content: truncate_output(text), is_error: false, images: Vec::new() },
            // The picture is the answer: it goes to the model as it is.
            Ok(Answer::Picture(out)) => out,
            Err(e) => text_output(Err(e)),
        }
    }

    fn specs(&self) -> Vec<ToolSpec> {
        let key = self.specs_key();
        let builtin = {
            let mut cache = self.specs_cache.lock();
            match cache.as_ref() {
                Some(cached) if cached.key == key => cached.specs.clone(),
                _ => {
                    let specs = self.measured_specs();
                    *cache = Some(SpecsCache { key, specs: specs.clone() });
                    specs
                }
            }
        };
        let mut specs = builtin;
        // Names a server chose, with the schemas it chose.
        specs.extend(self.mcp_manager.get_all_tool_specs());
        specs
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    #[serde(default)]
    files: Vec<ReadItem>,
}

#[derive(Deserialize)]
struct ReadItem {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

impl ReadArgs {
    fn read(self, cwd: &std::path::Path) -> Result<String, ToolError> {
        let mode = self.mode.as_deref().unwrap_or("auto").trim().to_ascii_lowercase();
        if mode == "outline" {
            if self.path.trim().is_empty() {
                return Err(ToolError::Other("read: mode outline needs one path".into()));
            }
            return outline::outline_file(cwd, &self.path);
        }
        if self.files.is_empty() {
            if self.path.trim().is_empty() {
                return Err(ToolError::Other("read needs a path, or files to read several".into()));
            }
            return fs_tools::read_file(cwd, &self.path, self.offset.unwrap_or(0), self.limit.unwrap_or(2000));
        }
        let mut items: Vec<(String, Option<usize>, Option<usize>)> = Vec::new();
        if !self.path.trim().is_empty() {
            items.push((self.path, self.offset, self.limit));
        }
        items.extend(self.files.into_iter().map(|f| (f.path, f.offset, f.limit)));
        let share = (2000 / items.len().max(1)).max(1);
        let items: Vec<(String, usize, usize)> = items.into_iter().map(|(p, o, l)| (p, o.unwrap_or(0), l.unwrap_or(share))).collect();
        fs_tools::read_files(cwd, &items)
    }
}

#[derive(Deserialize)]
struct SearchToolArgs {
    mode: String,
    path: Option<String>,
    pattern: Option<String>,
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
    limit: Option<usize>,
}

impl SearchToolArgs {
    async fn run(self, tools: &BuiltinTools) -> Result<String, ToolError> {
        let mode = self.mode.trim().to_ascii_lowercase();
        let cap = self.limit.unwrap_or(200).clamp(1, 1000);
        let text = match mode.as_str() {
            "list" => {
                let path = self.path.unwrap_or_else(|| ".".to_string());
                tools.off_runtime(move |cwd| fs_tools::list_dir(cwd, &path)).await?
            }
            "files" => {
                let pattern = self.pattern.ok_or_else(|| ToolError::Other("search mode files needs a pattern".into()))?;
                tools.off_runtime(move |cwd| fs_tools::glob_files(cwd, &pattern)).await?
            }
            "content" => {
                let pattern = self.pattern.ok_or_else(|| ToolError::Other("search mode content needs a pattern".into()))?;
                let glob = self.glob;
                let case = self.case_insensitive;
                tools
                    .off_runtime(move |cwd| fs_tools::grep(cwd, &pattern, glob.as_deref(), case))
                    .await?
            }
            other => return Err(ToolError::Other(format!("search: unknown mode '{other}'; use list, files or content"))),
        };
        Ok(cap_lines(text, cap))
    }
}

fn cap_lines(text: String, limit: usize) -> String {
    let mut lines = text.lines();
    let kept: Vec<&str> = lines.by_ref().take(limit).collect();
    if lines.next().is_none() {
        return text;
    }
    let total = text.lines().count();
    format!("{}\n...[showing {limit} of {total} lines; narrow the search or raise limit]", kept.join("\n"))
}

#[derive(Deserialize)]
struct EditToolArgs {
    mode: String,
    #[serde(default)]
    path: String,
    content: Option<String>,
    #[serde(default)]
    edits: Vec<fs_tools::EditChunk>,
    /// Applied as one change.
    #[serde(default)]
    files: Vec<EditItem>,
    patch: Option<String>,
}

#[derive(Deserialize)]
struct EditItem {
    path: String,
    #[serde(default)]
    edits: Vec<fs_tools::EditChunk>,
}

impl EditToolArgs {
    /// `path` first, then `files`.
    fn targets(self) -> Vec<(String, Vec<fs_tools::EditChunk>)> {
        let mut targets = Vec::new();
        if !self.path.trim().is_empty() && !self.edits.is_empty() {
            targets.push((self.path, self.edits));
        }
        targets.extend(self.files.into_iter().map(|f| (f.path, f.edits)));
        targets
    }
}

#[derive(Deserialize)]
struct GitArgs {
    action: String,
    path: Option<String>,
    staged: Option<bool>,
    rev: Option<String>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WebArgs {
    action: String,
    query: Option<String>,
    count: Option<u32>,
    url: Option<String>,
}

#[derive(Deserialize)]
struct ChatReplyArgs {
    message_id: i64,
    text: String,
}

#[derive(Deserialize)]
struct HttpRequestArgs {
    method: Option<String>,
    url: String,
    #[serde(default)]
    headers: Vec<HttpHeader>,
    body: Option<String>,
}

#[derive(Deserialize)]
struct HttpHeader {
    name: String,
    value: String,
}

#[derive(Deserialize)]
struct SkillArgs {
    action: String,
    name: Option<String>,
}

#[derive(Deserialize)]
struct TaskArgs {
    action: String,
    task_id: Option<u32>,
}

#[derive(Deserialize)]
struct ShellArgs {
    command: Option<String>,
    background: Option<bool>,
    timeout_ms: Option<u64>,
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    pub(crate) fn tempdir() -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flashagent-tools-{}-{id}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create tempdir");
        dir
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_write_preview_names_the_file_relative_to_the_project() {
        // The approval card showed "--- a//tmp/.../main.rs" for an absolute path.
        let dir = testing::tempdir();
        std::fs::write(dir.join("main.rs"), "fn main() {}\n").unwrap();
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir.clone(),
            ..Default::default()
        })
        .expect("tools");
        let abs = dir.join("main.rs").to_string_lossy().to_string();
        let call = ToolCall {
            id: "c".into(),
            name: "edit".into(),
            args_json: serde_json::json!({"path": abs, "content": "fn main() { println!(); }\n"}).to_string(),
        };
        let diff = tools.preview_for(&call).expect("a preview");
        assert!(diff.contains("--- a/main.rs") && diff.contains("+++ b/main.rs"), "{diff}");
        assert!(!diff.contains("a//"), "{diff}");
    }
    #[tokio::test]
    async fn a_chat_reply_lands_in_the_outbox_and_the_toolset() {
        let dir = testing::tempdir();
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        let keep = sent.clone();
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir.clone(),
            reply_outbox: Some(Arc::new(move |id: i64, text: &str| {
                keep.lock().unwrap().push((id, text.to_string()));
            })),
            ..Default::default()
        })
        .expect("tools");
        // With the outbox wired the tool is on the table.
        assert!(tools.specs().iter().any(|s| s.name == "chat_reply"), "offered");
        let out = tools
            .execute(&batch_call(
                "chat_reply",
                serde_json::json!({"message_id": 42, "text": "держи"}),
            ))
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("msg:42"), "{}", out.content);
        assert_eq!(*sent.lock().unwrap(), vec![(42, "держи".to_string())]);
    }
    #[tokio::test]
    async fn chat_reply_without_an_outbox_is_not_offered_and_refused() {
        let dir = testing::tempdir();
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir,
            ..Default::default()
        })
        .expect("tools");
        assert!(!tools.specs().iter().any(|s| s.name == "chat_reply"), "not offered");
        let out = tools
            .execute(&batch_call(
                "chat_reply",
                serde_json::json!({"message_id": 7, "text": "привет"}),
            ))
            .await;
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("not available"), "{}", out.content);
    }
    #[test]
    fn edit_file_approval_card_gets_a_diff() {
        // Without a preview the user approves blind.
        let dir = std::env::temp_dir().join(format!("fa-preview-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/parser.rs"), "fn a() {}\npub fn parse_duration() {}\n").unwrap();

        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir.clone(),
            ..Default::default()
        })
        .unwrap();
        let call = ToolCall {
            id: "1".into(),
            name: "edit".into(),
            args_json: r#"{"path":"src/parser.rs","edits":[{"old_string":"pub fn parse_duration() {}","new_string":"/// A bare number means minutes.\npub fn parse_duration() {}"}]}"#.into(),
        };
        let diff = tools.write_preview(&call);
        let _ = std::fs::remove_dir_all(&dir);
        let diff = diff.expect("edit_file must preview as a diff");
        assert!(diff.contains("+/// A bare number means minutes."), "{diff}");
    }

    fn batch_tools(tag: &str, files: &[(&str, &str)]) -> (PathBuf, BuiltinTools) {
        let dir = std::env::temp_dir().join(format!("fa-batch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, text) in files {
            std::fs::write(dir.join(name), text).unwrap();
        }
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir.clone(),
            ..Default::default()
        })
        .unwrap();
        (dir, tools)
    }

    fn batch_call(name: &str, args: serde_json::Value) -> ToolCall {
        let mut args = args;
        if name == "edit" {
            if let Some(obj) = args.as_object_mut() {
                obj.entry("mode").or_insert(serde_json::json!("replace"));
            }
        }
        ToolCall { id: "t".into(), name: name.into(), args_json: args.to_string() }
    }

    #[test]
    fn an_edit_that_cannot_apply_is_known_before_anyone_approves_it() {
        use flashagent_core::WritePreview;
        let (dir, tools) = batch_tools("doomed", &[("a.rs", "fn one() {}\n")]);
        let missing = batch_call("edit", serde_json::json!({ "path": "a.rs", "edits": [ { "old_string": "fn two() {}", "new_string": "x" } ] }));
        let fine = batch_call("edit", serde_json::json!({ "path": "a.rs", "edits": [ { "old_string": "fn one() {}", "new_string": "x" } ] }));
        let outside = batch_call("edit", serde_json::json!({ "path": "../a.rs", "edits": [ { "old_string": "nope", "new_string": "x" } ] }));
        let home = batch_call("edit", serde_json::json!({ "path": "~/.gitconfig", "edits": [ { "old_string": "nope", "new_string": "x" } ] }));
        let twice = batch_call("edit", serde_json::json!({ "files": [
            { "path": "a.rs", "edits": [ { "old_string": "fn one() {}", "new_string": "fn two() {}" } ] },
            { "path": "a.rs", "edits": [ { "old_string": "fn two() {}", "new_string": "fn three() {}" } ] }
        ] }));
        let doomed = tools.doomed(&missing);
        let (fine, outside, home, twice) = (tools.doomed(&fine), tools.doomed(&outside), tools.doomed(&home), tools.doomed(&twice));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(doomed.as_deref().is_some_and(|e| e.contains("closest line is 1")), "{doomed:?}");
        assert_eq!(fine, None);
        assert_eq!(outside, None, "a file outside the project is not read to say so");
        assert_eq!(home, None, "a file under ~ is not read to say so");
        assert_eq!(twice, None, "a second edit of the same file applies to the first one's result");
    }

    #[test]
    fn a_file_named_twice_in_a_batch_is_previewed_and_checked_as_written() {
        let (dir, tools) = batch_tools("twice", &[("a.txt", "one\n")]);
        let hidden = batch_call("edit", serde_json::json!({ "files": [
            { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "two" } ] },
            { "path": "./a.txt", "edits": [ { "old_string": "two", "new_string": "rm -rf" } ] }
        ] }));
        let preview = tools.write_preview(&hidden).unwrap_or_default();
        let doomed = tools.doomed(&hidden);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(preview.contains("+rm -rf"), "the card must show what will be written: {preview}");
        assert!(!preview.contains("+two"), "{preview}");
        assert_eq!(doomed, None, "./a.txt is the same file as a.txt");
    }

    #[tokio::test]
    async fn a_batch_edit_changes_every_file_it_names() {
        let (dir, tools) = batch_tools("ok", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let out = tools
            .execute(&batch_call("edit", serde_json::json!({ "files": [
                { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "ONE" } ] },
                { "path": "b.txt", "edits": [ { "old_string": "two", "new_string": "TWO" } ] }
            ] })))
            .await;
        let (a, b) = (std::fs::read_to_string(dir.join("a.txt")).unwrap(), std::fs::read_to_string(dir.join("b.txt")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!((a.as_str(), b.as_str()), ("ONE\n", "TWO\n"));
        assert!(out.content.contains("2 edits to 2 files"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_batch_edit_with_one_edit_that_does_not_fit_changes_no_file() {
        let (dir, tools) = batch_tools("atomic", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let out = tools
            .execute(&batch_call("edit", serde_json::json!({ "files": [
                { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "ONE" } ] },
                { "path": "b.txt", "edits": [ { "old_string": "not in the file", "new_string": "x" } ] }
            ] })))
            .await;
        let (a, b) = (std::fs::read_to_string(dir.join("a.txt")).unwrap(), std::fs::read_to_string(dir.join("b.txt")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(out.is_error, "{}", out.content);
        assert!(out.content.contains("nothing was changed") && out.content.contains("b.txt"), "{}", out.content);
        assert_eq!((a.as_str(), b.as_str()), ("one\n", "two\n"), "a half-applied batch was written");
    }

    #[tokio::test]
    async fn a_batch_edit_is_previewed_whole() {
        let (dir, tools) = batch_tools("preview", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let diff = tools.write_preview(&batch_call("edit", serde_json::json!({ "files": [
            { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "ONE" } ] },
            { "path": "b.txt", "edits": [ { "old_string": "two", "new_string": "TWO" } ] }
        ] })));
        let _ = std::fs::remove_dir_all(&dir);
        let diff = diff.expect("a batch edit previews as a diff");
        assert!(diff.contains("+ONE") && diff.contains("+TWO"), "{diff}");
    }

    #[tokio::test]
    async fn a_prepared_batch_rejects_changes_made_after_the_approval_diff() {
        let (dir, tools) = batch_tools("stale-prepared", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let call = batch_call("edit", serde_json::json!({ "files": [
            { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "ONE" } ] },
            { "path": "b.txt", "edits": [ { "old_string": "two", "new_string": "TWO" } ] }
        ] }));
        let prepared = tools.prepare(&call).expect("a prepared edit");
        assert!(prepared.preview().unwrap().contains("+TWO"));
        std::fs::write(dir.join("b.txt"), "two\nexternal\n").unwrap();
        let out = prepared.execute().await;
        assert!(out.is_error && out.content.contains("nothing was changed"), "{}", out.content);
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "one\n");
        assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "two\nexternal\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_or_oversized_batches_never_preview_only_the_valid_files() {
        let (dir, tools) = batch_tools("invalid-preview", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let bad = batch_call("edit", serde_json::json!({ "files": [
            { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "ONE" } ] },
            { "path": "b.txt", "edits": [ { "old_string": "missing", "new_string": "TWO" } ] }
        ] }));
        assert!(tools.write_preview(&bad).is_none(), "a partial diff would hide the failed edit");
        assert!(tools.prepare(&bad).unwrap().error().unwrap().contains("b.txt"));
        let files: Vec<_> = (0..21).map(|_| serde_json::json!({
            "path": "a.txt", "edits": [{"old_string": "one", "new_string": "one"}]
        })).collect();
        let large = batch_call("edit", serde_json::json!({"files": files}));
        assert!(tools.write_preview(&large).is_none());
        assert!(tools.doomed(&large).unwrap().contains("at most 20"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn several_files_are_read_in_one_call_and_a_missing_one_does_not_stop_the_rest() {
        let (dir, tools) = batch_tools("read", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let out = tools
            .execute(&batch_call("read", serde_json::json!({ "files": [ { "path": "a.txt" }, { "path": "nope.txt" }, { "path": "b.txt" } ] })))
            .await;
        let none = tools
            .execute(&batch_call("read", serde_json::json!({ "paths": [ "nope.txt", "gone.txt" ] })))
            .await;
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!out.is_error, "{}", out.content);
        for part in ["=== a.txt ===", "one", "=== nope.txt ===", "error:", "=== b.txt ===", "two"] {
            assert!(out.content.contains(part), "missing {part:?} in:\n{}", out.content);
        }
        assert!(none.is_error, "a read where no file could be read is an error: {}", none.content);
    }

    use super::*;

    #[tokio::test]
    async fn long_error_results_are_truncated_too() {
        let tools = BuiltinTools::new(BuiltinToolsConfig { cwd: testing::tempdir(), ..Default::default() }).unwrap();
        let call = ToolCall { id: "t".into(), name: "read".into(), args_json: "x".repeat(MAX_OUTPUT_BYTES) };
        let out = tools.execute(&call).await;
        assert!(out.is_error);
        assert!(out.content.contains("truncated"));
        assert!(out.content.chars().count() < MAX_OUTPUT_CHARS + 100, "{} chars retained", out.content.chars().count());
    }

    #[tokio::test]
    async fn hostile_args_become_errors_not_panics() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            ..Default::default()
        })
        .unwrap();
        for args in ["{ not json", "", r#"{"path": 42}"#] {
            let out = tools
                .execute(&ToolCall { id: "t".into(), name: "read".into(), args_json: args.into() })
                .await;
            assert!(out.is_error, "args {args:?} must error, got {out:?}");
        }
        let out = tools
            .execute(&ToolCall { id: "t".into(), name: "rm_rf_everything".into(), args_json: "{}".into() })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("unknown tool"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_read_that_waits_leaves_the_runtime_free() {
        // Opening a named pipe waits until something writes to it: a stand-in for
        // a grep over a huge tree or a slow disk.
        let dir = testing::tempdir();
        let pipe = dir.join("pipe");
        let c_path = std::ffi::CString::new(pipe.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path; mkfifo only creates the file.
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) }, 0, "mkfifo failed");
        let writer = {
            let pipe = pipe.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                std::fs::write(pipe, "written late\n")
            })
        };
        let tools = BuiltinTools::new(BuiltinToolsConfig { cwd: dir, ..Default::default() }).unwrap();
        let call = ToolCall { id: "t".into(), name: "read".into(), args_json: r#"{"path":"pipe"}"#.into() };
        let read = tools.execute(&call);
        tokio::pin!(read);
        // A single-threaded runtime: a read done inline would hold it until the
        // writer came, and the read would finish first.
        tokio::select! {
            biased;
            _ = &mut read => panic!("the read held the runtime until it finished"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        let out = read.await;
        writer.join().unwrap().unwrap();
        assert!(out.content.contains("written late"), "{}", out.content);
    }

    #[tokio::test]
    async fn missing_file_is_tool_error() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            ..Default::default()
        })
        .unwrap();
        let out = tools
            .execute(&ToolCall {
                id: "t".into(),
                name: "read".into(),
                args_json: r#"{"path":"nope.txt"}"#.into(),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("error:"));
    }

    #[test]
    fn the_whole_toolset_is_offered() {
        // One list for every model: the budget trims it if it must, the owner
        // switches tools off by hand. There is no profile to choose any more.
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            ..Default::default()
        })
        .unwrap();
        let specs = tools.specs();
        assert_eq!(specs.len(), 13); // the unified set, web on, agent added one level up
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"user_interaction"));
        assert!(names.contains(&"search"));
        assert!(names.contains(&"git"));
        assert!(names.contains(&"edit"));
        assert!(names.contains(&"env_info"));
        assert!(names.contains(&"memory"));
        assert!(names.contains(&"web"));
        assert!(names.contains(&"http_request"));
    }

    #[tokio::test]
    async fn plan_is_offered_and_works_in_ordinary_chat_as_well_as_a_goal() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            ..Default::default()
        })
        .unwrap();
        let before = tools.specs();
        // It used to refuse itself here, so a model that had planned a long
        // task in ordinary chat had nowhere to show the plan.
        let out = tools
            .execute(&ToolCall {
                id: "t".into(),
                name: "plan".into(),
                args_json: r#"{"action":"create","items":[{"text":"a"}]}"#.into(),
            })
            .await;
        assert!(!out.is_error, "a plan outside /goal must be kept: {}", out.content);
        assert!(out.content.contains("1 step"), "{}", out.content);
        // A step is marked without sending the whole list again.
        let out = tools
            .execute(&ToolCall {
                id: "t2".into(),
                name: "plan".into(),
                args_json: r#"{"action":"done","steps":["a"]}"#.into(),
            })
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("Marked"), "{}", out.content);

        tools.set_goal_mode(true);
        let names = |specs: Vec<ToolSpec>| specs.into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names(before), names(tools.specs()), "starting a goal must not change the tool list the server has cached");
        let out = tools
            .execute(&ToolCall {
                id: "t3".into(),
                name: "plan".into(),
                args_json: r#"{"action":"change","items":[{"text":"a","status":"completed"}]}"#.into(),
            })
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("1 done"), "{}", out.content);
    }

    #[tokio::test]
    async fn web_tools_turned_off_in_settings_are_neither_offered_nor_run() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            web_enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
        let out = tools.execute(&ToolCall {
            id: "1".into(),
            name: "web".into(),
            args_json: r#"{"action":"search","query":"rust"}"#.into(),
        }).await;
        assert!(out.is_error);
        assert!(out.content.contains("turned off in Settings"), "{}", out.content);
        assert!(!tools.specs().iter().any(|s| s.name == "http_request"), "a tool that is off must not be offered");

        tools.set_web_enabled(true);
        let specs = tools.specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"web"));
        assert!(names.contains(&"http_request"));
    }

    #[test]
    fn adaptive_toolset_per_context_window() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            web_enabled: Some(false),
            context_window: Some(32_000), // a small window no longer trims the toolset
            ..Default::default()
        })
        .unwrap();
        let specs = tools.specs();
        assert_eq!(specs.len(), 11); // Auto keeps the full toolset even at 32k (no window-based downgrade)
        assert!(specs.iter().any(|s| s.name == "edit"));

        tools.set_context_window(Some(65_536));
        // What the app has measured: the schemas are given what a tier has left
        // after the prompt, not after its ceiling.
        tools.set_prompt_tokens(3_900);
        let specs_large = tools.specs();
        assert_eq!(specs_large.len(), 11);
        assert!(specs_large.iter().any(|s| s.name == "edit"));
    }

    #[test]
    fn a_window_that_cannot_hold_the_prompt_keeps_the_tools_a_task_needs() {
        // A prompt that fills its whole ceiling on a 64k window leaves nothing
        // for schemas. What is left must still be the set a task happens with,
        // not an agent with no way to read or write anything.
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            web_enabled: Some(false),
            context_window: Some(65_536),
            ..Default::default()
        })
        .unwrap();
        tools.set_prompt_tokens(flashagent_core::budget::PROMPT_CEILING);
        let specs = tools.specs();
        for needed in ["read", "edit", "search", "run_shell", "user_interaction"] {
            assert!(specs.iter().any(|s| s.name == needed), "{needed} is gone at {specs:?}");
        }
        // And what did not fit is reported rather than silently dropped.
        assert!(!specs.iter().any(|s| s.name == "http_request"), "the optional end still goes first");
    }

    #[tokio::test]
    async fn twenty_files_share_the_line_budget_so_the_last_one_is_read_too() {
        // The 2000 lines are shared out. The share used to have a floor of
        // 200 lines, which undid the division: twenty files were read at 200
        // lines each, four thousand lines, and the end of the answer was cut
        // off by the output limit.
        let dir = testing::tempdir();
        let files: Vec<String> = (0..20)
            .map(|i| {
                let name = format!("f{i}.txt");
                std::fs::write(dir.join(&name), "x\n".repeat(400)).unwrap();
                name
            })
            .collect();
        let tools = BuiltinTools::new(BuiltinToolsConfig { cwd: dir, ..Default::default() }).unwrap();
        let listed: Vec<serde_json::Value> =
            files.iter().map(|path| serde_json::json!({ "path": path })).collect();
        let out = tools
            .execute(&ToolCall {
                id: "t".into(),
                name: "read".into(),
                args_json: serde_json::json!({ "files": listed }).to_string(),
            })
            .await;
        assert!(!out.is_error, "{}", out.content);
        for name in &files {
            assert!(out.content.contains(&format!("=== {name} ===")), "{name} is missing from the answer");
        }
        let lines = out.content.lines().filter(|l| l.contains('\t')).count();
        assert!(lines <= 2000, "{lines} lines were read, the budget is 2000");
    }

    #[test]
    fn the_tool_list_is_measured_once_and_kept() {
        // Measuring it means tokenizing every schema, and the list is asked
        // for on every tool call.
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            ..Default::default()
        })
        .unwrap();
        let first = tools.specs();
        let second = tools.specs();
        assert_eq!(first.len(), second.len());
        assert_eq!(tools.specs_measured(), 1, "the schemas were measured again for the same list");
        tools.set_context_window(Some(1_000_000));
        tools.specs();
        assert_eq!(tools.specs_measured(), 1, "flat schema budgets must not remeasure on a window change");

        // Something that changes the list is measured again.
        tools.set_model_name("gpt-4");
        let third = tools.specs();
        assert_eq!(tools.specs_measured(), 2, "a new model must be measured with its own tokenizer");
        assert_eq!(first.len(), third.len());
    }

    #[test]
    fn a_tool_that_is_switched_off_is_gone_from_the_kept_list() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            ..Default::default()
        })
        .unwrap();
        assert!(tools.specs().iter().any(|s| s.name == "web"));
        tools.set_disabled_tools(vec!["web".into()]);
        let specs = tools.specs();
        assert!(!specs.iter().any(|s| s.name == "web"), "a tool that is off is still offered");
        assert_eq!(tools.specs_measured(), 2, "the list must be measured again after the change");
    }

    #[test]
    fn truncate_output_caps_and_marks() {        let long = "x".repeat(MAX_OUTPUT_CHARS + 10);
        let out = truncate_output(long);
        assert!(out.contains("[truncated"));
        assert_eq!(out.chars().count(), MAX_OUTPUT_CHARS + "\n...[truncated at 32000 chars]".len());
        let exact = "ё".repeat(MAX_OUTPUT_CHARS);
        assert_eq!(truncate_output(exact.clone()), exact, "a text exactly at the cap is whole");
        let over = truncate_output("ё".repeat(MAX_OUTPUT_CHARS + 1));
        assert!(over.starts_with(&exact) && over[exact.len()..].starts_with("\n...[truncated"), "cut by characters, not bytes");
    }

    #[test]
    fn parse_args_self_healing_python_dict_and_unwrapping() {
        let py_dict = "{'path': 'src/main.rs', 'offset': 10}";
        let read: ReadArgs = parse_args(py_dict, "read_file").unwrap();
        assert_eq!(read.path, "src/main.rs");
        assert_eq!(read.offset, Some(10));

        let wrapped = r#"{"arguments": {"path": "src/lib.rs", "offset": 5}}"#;
        let read2: ReadArgs = parse_args(wrapped, "read_file").unwrap();
        assert_eq!(read2.path, "src/lib.rs");
        assert_eq!(read2.offset, Some(5));

        let single_key = r#"{"read_file": {"path": "README.md"}}"#;
        let read3: ReadArgs = parse_args(single_key, "read_file").unwrap();
        assert_eq!(read3.path, "README.md");
    }

    #[test]
    fn a_call_in_another_agents_argument_names_is_read_as_this_tools_own() {
        let edit: EditToolArgs = parse_args(r#"{"filePath":"a.rs","oldString":"x","newString":"y"}"#, "edit").unwrap();
        assert_eq!(edit.path, "a.rs");
        assert_eq!(edit.mode, "replace");
        assert_eq!(edit.edits.len(), 1);
        assert_eq!((edit.edits[0].old_string.as_str(), edit.edits[0].new_string.as_str()), ("x", "y"));

        let shell: ShellArgs = parse_args(r#"{"cmd":"ls"}"#, "run_shell").unwrap();
        assert_eq!(shell.command.as_deref(), Some("ls"));

        let write: EditToolArgs = parse_args(r#"{"mode":"write","file_path":"b.txt","contents":"hi"}"#, "edit").unwrap();
        assert_eq!((write.path.as_str(), write.content.as_deref()), ("b.txt", Some("hi")));
    }
}
