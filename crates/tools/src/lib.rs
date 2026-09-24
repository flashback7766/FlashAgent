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
pub mod subagents;
pub mod web;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flashagent_core::{ToolExec, ToolOutput, ToolsetProfile, WritePreview};
use flashagent_llm::{ToolCall, ToolSpec};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use shell::ShellRegistry;

pub use ask_user::{QuestionGate, GOAL_QUESTION_TIMEOUT};
pub use subagents::{agent_tools, BuiltinSubagentFactory, CompositeTools, ToolSubset};

/// Hard cap on a tool result fed back to the model.
const MAX_OUTPUT_CHARS: usize = 32_000;

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

/// Cut in place, looking no further than the cut: a diff or a log can be
/// megabytes.
fn truncate_output(mut text: String) -> String {
    if let Some((cut, _)) = text.char_indices().nth(MAX_OUTPUT_CHARS) {
        text.truncate(cut);
        text.push_str(&format!("\n...[truncated at {MAX_OUTPUT_CHARS} chars]"));
    }
    text
}

#[derive(Default)]
pub struct BuiltinToolsConfig {
    /// For all filesystem tools and the shell.
    pub cwd: PathBuf,
    /// Without it `web_search` uses DuckDuckGo.
    pub brave_api_key: Option<String>,
    pub question_gate: Option<Arc<dyn QuestionGate>>,
    /// Set while `/goal` runs.
    pub is_goal_mode: Option<Arc<AtomicBool>>,
    pub toolset_profile: Option<ToolsetProfile>,
    pub web_enabled: Option<bool>,
    /// Small windows get fewer tools.
    pub context_window: Option<usize>,
    pub mcp_manager: Option<Arc<mcp::McpManager>>,
}

/// One per session.
pub struct BuiltinTools {
    cwd: PathBuf,
    shells: ShellRegistry,
    http: reqwest::Client,
    brave_key: Option<String>,
    question_gate: Option<Arc<dyn QuestionGate>>,
    is_goal_mode: Arc<AtomicBool>,
    toolset_profile: std::sync::RwLock<ToolsetProfile>,
    web_enabled: Arc<AtomicBool>,
    context_window: Arc<std::sync::RwLock<Option<usize>>>,
    /// Set from what the server said about the model.
    vision_supported: Arc<std::sync::atomic::AtomicBool>,
    mcp_manager: Arc<mcp::McpManager>,
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
            cwd: config.cwd,
            shells: ShellRegistry::new(),
            http,
            brave_key: config.brave_api_key,
            question_gate: config.question_gate,
            is_goal_mode: config.is_goal_mode.unwrap_or_else(|| Arc::new(AtomicBool::new(false))),
            toolset_profile: std::sync::RwLock::new(config.toolset_profile.unwrap_or(ToolsetProfile::Auto)),
            web_enabled: Arc::new(AtomicBool::new(config.web_enabled.unwrap_or(true))),
            context_window: Arc::new(std::sync::RwLock::new(config.context_window)),
            vision_supported: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            mcp_manager,
        })
    }

    pub fn mcp_manager(&self) -> Arc<mcp::McpManager> {
        self.mcp_manager.clone()
    }

    pub fn set_goal_mode(&self, active: bool) {
        self.is_goal_mode.store(active, Ordering::Relaxed);
    }

    pub fn set_toolset_profile(&self, profile: ToolsetProfile) {
        if let Ok(mut lock) = self.toolset_profile.write() {
            *lock = profile;
        }
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

    /// Decides how many tools are offered.
    pub fn set_context_window(&self, window: Option<usize>) {
        if let Ok(mut lock) = self.context_window.write() {
            *lock = window;
        }
    }

    /// Relative to the project when inside it; models often return absolute paths.
    fn shown_path(&self, path: &str) -> String {
        std::path::Path::new(path)
            .strip_prefix(&self.cwd)
            .map(|rel| rel.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string())
    }

    /// `None` when the call is not a write or the file cannot be read.
    fn preview_for(&self, call: &ToolCall) -> Option<String> {
        let v = flashagent_llm::effective_args(&call.args_json, &call.name)?;
        match call.name.as_str() {
            "write_file" => {
                let path = v.get("path")?.as_str()?;
                let new = v.get("content")?.as_str()?;
                let old = fs_tools::read_raw(&self.cwd, path);
                Some(flashagent_core::unified(old.as_deref(), new, &self.shown_path(path), 3))
            }
            "edit_file" => {
                let args: EditArgs = serde_json::from_value(v.clone()).ok()?;
                // A batch is shown whole, one diff per file. A file named twice
                // (also as `./a.txt`) is shown from its original to what both
                // parts make of it, as edit_files writes it: previewing each part
                // on its own hid the second one.
                let mut files: Vec<(std::path::PathBuf, String, String, Option<String>)> = Vec::new();
                let edited = |path: &str, text: String, edits: &[fs_tools::EditChunk]| fs_tools::apply_edits(path, text, edits).ok().map(|(new, _)| new);
                for (path, edits) in args.targets() {
                    let key = fs_tools::same_file_key(&self.cwd, &path);
                    match files.iter_mut().find(|f| f.0 == key) {
                        Some(earlier) => earlier.3 = earlier.3.take().and_then(|text| edited(&path, text, &edits)),
                        None => {
                            if let Some(old) = fs_tools::read_raw(&self.cwd, &path) {
                                let new = edited(&path, old.clone(), &edits);
                                files.push((key, path, old, new));
                            }
                        }
                    }
                }
                let diffs: Vec<String> = files
                    .iter()
                    .filter_map(|(_, path, old, new)| Some(flashagent_core::unified(Some(old), new.as_ref()?, &self.shown_path(path), 3)))
                    .collect();
                if diffs.is_empty() {
                    None
                } else {
                    Some(diffs.join("\n"))
                }
            }
            "patch_file" => {
                let path = v.get("path")?.as_str()?;
                let patch_text = v.get("patch")?.as_str()?;
                patch::preview_patch(&self.cwd, path, patch_text)
            }
            _ => None,
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

    async fn dispatch(&self, call: &ToolCall) -> Result<String, ToolError> {
        match call.name.as_str() {
            "read_file" => {
                let a: ReadArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| a.read(cwd)).await
            }
            "write_file" => {
                let a: WriteArgs = parse_args(&call.args_json, &call.name)?;
                fs_tools::write_file(&self.cwd, &a.path, &a.content)
            }
            "edit_file" => {
                let a: EditArgs = parse_args(&call.args_json, &call.name)?;
                if a.files.is_empty() {
                    if a.path.trim().is_empty() || a.edits.is_empty() {
                        return Err(ToolError::Other("edit_file needs a path and edits, or files to edit several".into()));
                    }
                    return fs_tools::edit_file(&self.cwd, &a.path, &a.edits);
                }
                fs_tools::edit_files(&self.cwd, &a.targets())
            }
            "patch_file" => {
                let a: PatchArgs = parse_args(&call.args_json, &call.name)?;
                patch::patch_file(&self.cwd, &a.path, &a.patch)
            }
            "list_dir" => {
                let a: ListArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| fs_tools::list_dir(cwd, a.path.as_deref().unwrap_or("."))).await
            }
            "glob" => {
                let a: GlobArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| fs_tools::glob_files(cwd, &a.pattern)).await
            }
            "grep" => {
                let a: GrepArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| fs_tools::grep(cwd, &a.pattern, a.glob.as_deref(), a.case_insensitive)).await
            }
            "outline_file" => {
                let a: OutlineArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| outline::outline_file(cwd, &a.path)).await
            }
            "git_status" => {
                let a: GitStatusArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| git::git_status(cwd, a.path.as_deref())).await
            }
            "git_diff" => {
                let a: GitDiffArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| git::git_diff(cwd, a.staged.unwrap_or(false), a.path.as_deref())).await
            }
            "run_shell" => {
                let a: ShellArgs = parse_args(&call.args_json, &call.name)?;
                self.run_shell(a).await
            }
            "env_info" => self.off_runtime(env_tools::env_info).await,
            "ask_user" => {
                let a: ask_user::AskUserArgs = parse_args(&call.args_json, &call.name)?;
                ask_user::run_ask_user(self.question_gate.as_ref(), &self.is_goal_mode, a).await
            }
            "update_plan" => plan_tool::run_update_plan(&self.is_goal_mode, &call.args_json),
            "memory_read" => {
                let a: memory_tools::MemoryReadArgs = parse_args(&call.args_json, &call.name)?;
                self.off_runtime(move |cwd| memory_tools::memory_read(cwd, a)).await
            }
            "memory_create" => {
                let a: memory_tools::MemoryWriteArgs = parse_args(&call.args_json, &call.name)?;
                memory_tools::memory_create(&self.cwd, &self.is_goal_mode, a)
            }
            "memory_update" => {
                let a: memory_tools::MemoryWriteArgs = parse_args(&call.args_json, &call.name)?;
                memory_tools::memory_update(&self.cwd, &self.is_goal_mode, a)
            }
            "memory_remove" => {
                let a: memory_tools::MemoryRemoveArgs = parse_args(&call.args_json, &call.name)?;
                memory_tools::memory_remove(&self.cwd, &self.is_goal_mode, a)
            }
            "web_fetch" => {
                if !self.web_enabled.load(Ordering::Relaxed) {
                    return Err(ToolError::Other("web_fetch is turned off in Settings (LLM & Reasoning → Web Tools).".into()));
                }
                let a: FetchArgs = parse_args(&call.args_json, &call.name)?;
                web::fetch_text(&a.url).await
            }
            "web_search" => {
                if !self.web_enabled.load(Ordering::Relaxed) {
                    return Err(ToolError::Other("web_search is turned off in Settings (LLM & Reasoning → Web Tools).".into()));
                }
                let a: SearchArgs = parse_args(&call.args_json, &call.name)?;
                let count = a.count.unwrap_or(5);
                let key = self.brave_key.as_deref().map(str::trim).filter(|k| !k.is_empty());
                match key {
                    // A rejected, expired or exhausted key falls back to the free search.
                    Some(key) => match web::search(&self.http, key, &a.query, count).await {
                        Ok(found) => Ok(found),
                        Err(_) => web::search_free(&self.http, &a.query, count).await,
                    },
                    None => web::search_free(&self.http, &a.query, count).await,
                }
            }
            other => {
                if other.starts_with("mcp__") || self.mcp_manager.has_tool(other) {
                    self.mcp_manager.call_tool(other, &call.args_json).await
                } else {
                    Err(ToolError::Other(format!("unknown tool: {other}")))
                }
            }
        }
    }

    async fn run_shell(&self, a: ShellArgs) -> Result<String, ToolError> {
        if let Some(id) = a.task_id {
            return if a.kill { self.shells.kill(id) } else { self.shells.status(id) };
        }
        let cmd = a
            .command
            .ok_or_else(|| ToolError::Other("run_shell: `command` or `task_id` is required".into()))?;
        if a.background.unwrap_or(false) {
            self.shells.spawn_background(&cmd)
        } else {
            let timeout = Duration::from_millis(a.timeout_ms.unwrap_or(120_000));
            shell::run_foreground(&cmd, timeout).await
        }
    }
}

impl WritePreview for BuiltinTools {
    fn write_preview(&self, call: &ToolCall) -> Option<String> {
        self.preview_for(call)
    }

    /// Only when every file is inside the project, by the same rule the
    /// permissions use (`~`, `..` and symlinks resolved): the error quotes a
    /// line of the file, which for one outside would be a read nobody approved.
    fn doomed(&self, call: &ToolCall) -> Option<String> {
        if call.name != "edit_file" {
            return None;
        }
        let v = flashagent_llm::effective_args(&call.args_json, &call.name)?;
        let args: EditArgs = serde_json::from_value(v).ok()?;
        let targets = args.targets();
        if targets.iter().any(|(path, _)| !flashagent_core::path_is_inside(&self.cwd, path)) {
            return None;
        }
        // A file named twice in a batch, also as `./a.txt`, gets its second
        // edits on the result of the first.
        let mut texts: std::collections::HashMap<std::path::PathBuf, String> = std::collections::HashMap::new();
        for (path, edits) in targets {
            let key = fs_tools::same_file_key(&self.cwd, &path);
            let text = match texts.remove(&key) {
                Some(text) => text,
                None => fs_tools::read_raw(&self.cwd, &path)?,
            };
            match fs_tools::apply_edits(&path, text, &edits) {
                Ok((edited, _)) => texts.insert(key, edited),
                Err(e) => return Some(format!("error: {e}")),
            };
        }
        None
    }
}

#[async_trait]
impl ToolExec for BuiltinTools {
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        // The only tool whose result is a picture, not text.
        if call.name == "view_image" {
            let args = call.args_json.clone();
            let vision = self.vision_supported();
            let opened = self.off_runtime(move |cwd| Ok(image_tool::view_image(cwd, &args, vision))).await;
            return opened.unwrap_or_else(|e| ToolOutput { content: format!("error: {e}"), is_error: true, images: Vec::new() });
        }
        match self.dispatch(call).await {
            Ok(text) => ToolOutput { content: truncate_output(text), is_error: false, images: Vec::new() },
            Err(e) => ToolOutput { content: format!("error: {e}"), is_error: true, images: Vec::new() },
        }
    }

    fn specs(&self) -> Vec<ToolSpec> {
        let mut specs = vec![
            ToolSpec {
                name: "read_file".into(),
                description: "Read a text file with 1-based line numbers. For several files pass files instead of path (up to 20)".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string", "description": "File path, relative to working directory or absolute"}, "offset": {"type": "integer", "description": "0-based first line to read"}, "limit": {"type": "integer", "description": "Max lines to read (default 2000)"}, "files": {"type": "array", "description": "Several files in one call, each read like path/offset/limit", "items": {"type": "object", "properties": {"path": {"type": "string"}, "offset": {"type": "integer"}, "limit": {"type": "integer"}}, "required": ["path"]}}}, "required": ["header"]}"#.into(),
            },
            ToolSpec {
                name: "write_file".into(),
                description: "Create or fully overwrite a file".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string"}, "content": {"type": "string"}}, "required": ["header", "path", "content"]}"#.into(),
            },
            ToolSpec {
                name: "edit_file".into(),
                description: "Replace exact old_string matches in a file; new_string replaces old_string whole, so to insert a line keep its neighbour in new_string too. For several files pass files instead of path and edits (up to 20); all apply or none do".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string"}, "edits": {"type": "array", "items": {"type": "object", "properties": {"old_string": {"type": "string"}, "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}}, "required": ["old_string", "new_string"]}}, "files": {"type": "array", "description": "Several files changed as one change", "items": {"type": "object", "properties": {"path": {"type": "string"}, "edits": {"type": "array", "items": {"type": "object", "properties": {"old_string": {"type": "string"}, "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}}, "required": ["old_string", "new_string"]}}}, "required": ["path", "edits"]}}}, "required": ["header"]}"#.into(),
            },
            ToolSpec {
                name: "list_dir".into(),
                description: "List a directory; directory entries end with /".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string", "description": "Defaults to the working directory"}}, "required": ["header"]}"#.into(),
            },
            ToolSpec {
                name: "glob".into(),
                description: "Find files by glob pattern (e.g. src/**/*.rs), max 500 results".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "pattern": {"type": "string"}}, "required": ["header", "pattern"]}"#.into(),
            },
            ToolSpec {
                name: "grep".into(),
                description: "Search file contents by regex; returns path:line:text".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "pattern": {"type": "string"}, "glob": {"type": "string", "description": "Restrict to files matching this glob"}, "case_insensitive": {"type": "boolean"}}, "required": ["header", "pattern"]}"#.into(),
            },
            ToolSpec {
                name: "run_shell".into(),
                description: "Run a shell command (timeout_ms, default 120000). background:true returns a task_id; call again with task_id to poll, add kill:true to stop it".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "command": {"type": "string"}, "background": {"type": "boolean"}, "timeout_ms": {"type": "integer"}, "task_id": {"type": "integer", "description": "Poll or kill a background task"}, "kill": {"type": "boolean", "description": "With task_id: kill the task"}}, "required": ["header"]}"#.into(),
            },
            ToolSpec {
                name: "ask_user".into(),
                description: "The only way to ask the user anything. Give every question 2-6 short, concrete answers to pick from as options (answers, not more questions); the user can still type their own. Put several questions in questions".into(),
                parameters_json: r#"{"type": "object", "properties": {"question": {"type": "string", "description": "Single question to ask the user"}, "options": {"type": "array", "items": {"type": "string"}, "description": "2-6 answers to pick from"}, "multi_select": {"type": "boolean", "description": "Allow picking several options"}, "questions": {"type": "array", "items": {"type": "object", "properties": {"question": {"type": "string"}, "options": {"type": "array", "items": {"type": "string"}}, "multi_select": {"type": "boolean"}}, "required": ["question", "options"]}, "description": "Several questions, asked in order"}}, "required": []}"#.into(),
            },
            ToolSpec {
                name: "memory_read".into(),
                description: "Read one remembered fact in full by name, or list them. The index is already in your context.".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "scope": {"type": "string", "enum": ["project", "global", "all"], "description": "Which memory to look at; 'all' by default."}, "name": {"type": "string", "description": "Name of one memory to read in full. Omit to list what is remembered."}}, "required": ["header"]}"#.into(),
            },
        ];

        // Offered always, though it refuses itself outside /goal. The tool list is
        // part of the cached prompt: adding it when a goal started made the first goal
        // step re-read the whole conversation (8.5 s on 7k tokens on a laptop model).
        specs.push(ToolSpec {
            name: "update_plan".into(),
            description: "Only during /goal: record the whole step-by-step plan, again each time a step finishes or the plan changes. Does nothing outside /goal.".into(),
            parameters_json: r#"{"type": "object", "properties": {"steps": {"type": "array", "description": "The whole plan, in order", "items": {"type": "object", "properties": {"text": {"type": "string"}, "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}}, "required": ["text"]}}}, "required": ["steps"]}"#.into(),
        });

        // Offered unless the context window is too small for their schemas.
        let profile = self.toolset_profile.read().map(|p| *p).unwrap_or(ToolsetProfile::Auto);
        let ctx = self.context_window.read().ok().and_then(|c| *c);
        let effective_profile = match profile {
            ToolsetProfile::Compact => ToolsetProfile::Compact,
            ToolsetProfile::Full => ToolsetProfile::Full,
            ToolsetProfile::Auto => {
                if let Some(c) = ctx {
                    if c < 40_000 {
                        ToolsetProfile::Compact
                    } else {
                        ToolsetProfile::Auto
                    }
                } else {
                    ToolsetProfile::Auto
                }
            }
        };

        if effective_profile != ToolsetProfile::Compact {
            specs.push(ToolSpec {
                name: "patch_file".into(),
                description: "Apply a standard unified diff patch to a target file".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string", "description": "File path to patch"}, "patch": {"type": "string", "description": "Unified diff patch text with @@ hunks"}}, "required": ["header", "path", "patch"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "outline_file".into(),
                description: "Extract structural outline of a file (functions, structs, classes, traits, headings) with line numbers without reading the entire content".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string", "description": "File path to inspect"}}, "required": ["header", "path"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "git_status".into(),
                description: "Inspect git repository status: current branch, staged, unstaged, and untracked files".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string", "description": "Optional subpath filter"}}, "required": ["header"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "git_diff".into(),
                description: "View unified diff of working tree changes or staged changes".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "staged": {"type": "boolean", "description": "View staged/cached diff if true"}, "path": {"type": "string", "description": "Optional file path filter"}}, "required": ["header"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "env_info".into(),
                description: "Inspect OS platform, CPU architecture, working directory, and installed developer toolchain versions".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}}, "required": ["header"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "memory_create".into(),
                description: "Remember one fact across sessions: something the user told you about how they work, or a decision about this project and its reason. Not things the code, git history or docs already say. Disabled in autonomous /goal mode".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "title": {"type": "string", "description": "Short title; it becomes the memory's name."}, "content": {"type": "string", "description": "The fact itself, in full sentences, with the reason behind it when there is one."}, "description": {"type": "string", "description": "One line saying what this memory is about. It goes in the index that is loaded every turn, so make it specific."}, "type": {"type": "string", "enum": ["preference", "decision", "reference", "work"], "description": "preference = how the user likes to work; decision = a choice made about this project and why; reference = a pointer outwards (URL, ticket); work = ongoing goals or constraints."}, "scope": {"type": "string", "enum": ["project", "global"], "description": "Use 'global' for anything about the USER — how they work, what they prefer, corrections they gave you — so it follows them into every project. Use 'project' only for facts about this codebase. A sentence that starts with 'I' or 'the user' is global."}}, "required": ["header", "title", "content"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "memory_update".into(),
                description: "Correct something already remembered, when it turns out to be wrong or has changed. Disabled in autonomous /goal mode".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "title": {"type": "string", "description": "Name of the memory to correct."}, "content": {"type": "string", "description": "The corrected fact."}, "description": {"type": "string", "description": "One line saying what this memory is about. It goes in the index that is loaded every turn, so make it specific."}, "type": {"type": "string", "enum": ["preference", "decision", "reference", "work"], "description": "preference = how the user likes to work; decision = a choice made about this project and why; reference = a pointer outwards (URL, ticket); work = ongoing goals or constraints."}, "scope": {"type": "string", "enum": ["project", "global"], "description": "Use 'global' for anything about the USER — how they work, what they prefer, corrections they gave you — so it follows them into every project. Use 'project' only for facts about this codebase. A sentence that starts with 'I' or 'the user' is global."}}, "required": ["header", "title", "content"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "memory_remove".into(),
                description: "Forget a memory that turned out to be wrong or no longer applies. Disabled in autonomous /goal mode".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "title": {"type": "string", "description": "Name of the memory to forget."}, "scope": {"type": "string", "enum": ["project", "global"], "description": "Use 'global' for anything about the USER — how they work, what they prefer, corrections they gave you — so it follows them into every project. Use 'project' only for facts about this codebase. A sentence that starts with 'I' or 'the user' is global."}}, "required": ["header", "title"]}"#.into(),
            });
        // Only when the model can see: otherwise it calls the tool and apologises.
        if self.vision_supported() {
            specs.push(ToolSpec {
                name: "view_image".into(),
                description: "Look at an image in the project — a diagram, a screenshot, a mockup. The picture itself comes back, so describe what you see rather than guessing from the file name.".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "path": {"type": "string", "description": "Path to the image inside the project (png, jpg, gif, webp, bmp)."}}, "required": ["header", "path"]}"#.into(),
            });
        }

            if self.web_enabled.load(Ordering::Relaxed) {
                specs.push(ToolSpec {
                    name: "web_fetch".into(),
                    description: "Fetch a URL as text; HTML is reduced to plain text".into(),
                    parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "url": {"type": "string"}}, "required": ["header", "url"]}"#.into(),
                });
                specs.push(ToolSpec {
                    name: "web_search".into(),
                    description: "Search the web for documentation, articles, and solutions".into(),
                    parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "What this call is for, one short line in the user's language. Shown to the user instead of the call."}, "query": {"type": "string"}, "count": {"type": "integer", "description": "Results to return (default 5)"}}, "required": ["header", "query"]}"#.into(),
                });
            }
        }

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
        if self.files.is_empty() {
            if self.path.trim().is_empty() {
                return Err(ToolError::Other("read_file needs a path, or files to read several".into()));
            }
            return fs_tools::read_file(cwd, &self.path, self.offset.unwrap_or(0), self.limit.unwrap_or(2000));
        }
        let mut items: Vec<(String, Option<usize>, Option<usize>)> = Vec::new();
        if !self.path.trim().is_empty() {
            items.push((self.path, self.offset, self.limit));
        }
        items.extend(self.files.into_iter().map(|f| (f.path, f.offset, f.limit)));
        // The usual 2000 lines are shared out, so the output limit does not cut the
        // last file away.
        let share = (2000 / items.len().max(1)).max(200);
        let items: Vec<(String, usize, usize)> = items.into_iter().map(|(p, o, l)| (p, o.unwrap_or(0), l.unwrap_or(share))).collect();
        fs_tools::read_files(cwd, &items)
    }
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    #[serde(default)]
    path: String,
    #[serde(default)]
    edits: Vec<fs_tools::EditChunk>,
    /// Applied as one change.
    #[serde(default)]
    files: Vec<EditItem>,
}

#[derive(Deserialize)]
struct EditItem {
    path: String,
    #[serde(default)]
    edits: Vec<fs_tools::EditChunk>,
}

impl EditArgs {
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
struct PatchArgs {
    path: String,
    patch: String,
}

#[derive(Deserialize)]
struct ListArgs {
    path: Option<String>,
}

#[derive(Deserialize)]
struct GlobArgs {
    pattern: String,
}

#[derive(Deserialize)]
struct GrepArgs {
    pattern: String,
    glob: Option<String>,
    #[serde(default)]
    case_insensitive: bool,
}

#[derive(Deserialize)]
struct OutlineArgs {
    path: String,
}

#[derive(Deserialize)]
struct GitStatusArgs {
    path: Option<String>,
}

#[derive(Deserialize)]
struct GitDiffArgs {
    #[serde(default)]
    staged: Option<bool>,
    path: Option<String>,
}

#[derive(Deserialize)]
struct ShellArgs {
    command: Option<String>,
    background: Option<bool>,
    timeout_ms: Option<u64>,
    task_id: Option<u32>,
    #[serde(default)]
    kill: bool,
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    count: Option<u32>,
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
            name: "write_file".into(),
            args_json: serde_json::json!({"path": abs, "content": "fn main() { println!(); }\n"}).to_string(),
        };
        let diff = tools.preview_for(&call).expect("a preview");
        assert!(diff.contains("--- a/main.rs") && diff.contains("+++ b/main.rs"), "{diff}");
        assert!(!diff.contains("a//"), "{diff}");
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
            name: "edit_file".into(),
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
        ToolCall { id: "t".into(), name: name.into(), args_json: args.to_string() }
    }

    #[test]
    fn an_edit_that_cannot_apply_is_known_before_anyone_approves_it() {
        use flashagent_core::WritePreview;
        let (dir, tools) = batch_tools("doomed", &[("a.rs", "fn one() {}\n")]);
        let missing = batch_call("edit_file", serde_json::json!({ "path": "a.rs", "edits": [ { "old_string": "fn two() {}", "new_string": "x" } ] }));
        let fine = batch_call("edit_file", serde_json::json!({ "path": "a.rs", "edits": [ { "old_string": "fn one() {}", "new_string": "x" } ] }));
        let outside = batch_call("edit_file", serde_json::json!({ "path": "../a.rs", "edits": [ { "old_string": "nope", "new_string": "x" } ] }));
        let home = batch_call("edit_file", serde_json::json!({ "path": "~/.gitconfig", "edits": [ { "old_string": "nope", "new_string": "x" } ] }));
        let twice = batch_call("edit_file", serde_json::json!({ "files": [
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
        let hidden = batch_call("edit_file", serde_json::json!({ "files": [
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
            .execute(&batch_call("edit_file", serde_json::json!({ "files": [
                { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "ONE" } ] },
                { "path": "b.txt", "edits": [ { "old_string": "two", "new_string": "TWO" } ] }
            ] })))
            .await;
        let (a, b) = (std::fs::read_to_string(dir.join("a.txt")).unwrap(), std::fs::read_to_string(dir.join("b.txt")).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!out.is_error, "{}", out.content);
        assert_eq!((a.as_str(), b.as_str()), ("ONE\n", "TWO\n"));
        assert!(out.content.contains("2 file(s)"), "{}", out.content);
    }

    #[tokio::test]
    async fn a_batch_edit_with_one_edit_that_does_not_fit_changes_no_file() {
        let (dir, tools) = batch_tools("atomic", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let out = tools
            .execute(&batch_call("edit_file", serde_json::json!({ "files": [
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
        let diff = tools.write_preview(&batch_call("edit_file", serde_json::json!({ "files": [
            { "path": "a.txt", "edits": [ { "old_string": "one", "new_string": "ONE" } ] },
            { "path": "b.txt", "edits": [ { "old_string": "two", "new_string": "TWO" } ] }
        ] })));
        let _ = std::fs::remove_dir_all(&dir);
        let diff = diff.expect("a batch edit previews as a diff");
        assert!(diff.contains("+ONE") && diff.contains("+TWO"), "{diff}");
    }

    #[tokio::test]
    async fn several_files_are_read_in_one_call_and_a_missing_one_does_not_stop_the_rest() {
        let (dir, tools) = batch_tools("read", &[("a.txt", "one\n"), ("b.txt", "two\n")]);
        let out = tools
            .execute(&batch_call("read_file", serde_json::json!({ "files": [ { "path": "a.txt" }, { "path": "nope.txt" }, { "path": "b.txt" } ] })))
            .await;
        let none = tools
            .execute(&batch_call("read_file", serde_json::json!({ "paths": [ "nope.txt", "gone.txt" ] })))
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
    async fn hostile_args_become_errors_not_panics() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            ..Default::default()
        })
        .unwrap();
        for args in ["{ not json", "", r#"{"path": 42}"#] {
            let out = tools
                .execute(&ToolCall { id: "t".into(), name: "read_file".into(), args_json: args.into() })
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
        let call = ToolCall { id: "t".into(), name: "read_file".into(), args_json: r#"{"path":"pipe"}"#.into() };
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
                name: "read_file".into(),
                args_json: r#"{"path":"nope.txt"}"#.into(),
            })
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("error:"));
    }

    #[test]
    fn specs_include_tools_by_profile() {
        let tools_auto = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            toolset_profile: Some(ToolsetProfile::Auto),
            ..Default::default()
        })
        .unwrap();
        let specs_auto = tools_auto.specs();
        assert_eq!(specs_auto.len(), 20); // 10 core (update_plan included) + 8 extended + 2 web, on by default
        let names_auto: Vec<&str> = specs_auto.iter().map(|s| s.name.as_str()).collect();
        assert!(names_auto.contains(&"ask_user"));
        assert!(names_auto.contains(&"outline_file"));
        assert!(names_auto.contains(&"git_status"));
        assert!(names_auto.contains(&"git_diff"));
        assert!(names_auto.contains(&"patch_file"));
        assert!(names_auto.contains(&"env_info"));
        assert!(names_auto.contains(&"memory_read"));
        assert!(names_auto.contains(&"memory_create"));
        assert!(names_auto.contains(&"web_search"));
        assert!(names_auto.contains(&"web_fetch"));

        let tools_compact = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            toolset_profile: Some(ToolsetProfile::Compact),
            ..Default::default()
        })
        .unwrap();
        let specs_compact = tools_compact.specs();
        assert_eq!(specs_compact.len(), 10);
        let names_compact: Vec<&str> = specs_compact.iter().map(|s| s.name.as_str()).collect();
        assert!(names_compact.contains(&"ask_user"));
        assert!(!names_compact.contains(&"patch_file"));
    }

    #[tokio::test]
    async fn update_plan_is_always_offered_and_only_works_during_a_goal_run() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            toolset_profile: Some(ToolsetProfile::Auto),
            ..Default::default()
        })
        .unwrap();
        let before = tools.specs();
        let out = tools
            .execute(&ToolCall { id: "t".into(), name: "update_plan".into(), args_json: r#"{"steps":[{"text":"a"}]}"#.into() })
            .await;
        assert!(out.is_error, "must refuse itself outside /goal even if called");

        tools.set_goal_mode(true);
        let names = |specs: Vec<ToolSpec>| specs.into_iter().map(|s| s.name).collect::<Vec<_>>();
        assert_eq!(names(before), names(tools.specs()), "starting a goal must not change the tool list the server has cached");
        let out = tools
            .execute(&ToolCall { id: "t".into(), name: "update_plan".into(), args_json: r#"{"steps":[{"text":"a"}]}"#.into() })
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("1 step"), "{}", out.content);
    }

    #[tokio::test]
    async fn web_tools_turned_off_in_settings_are_neither_offered_nor_run() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            toolset_profile: Some(ToolsetProfile::Auto),
            web_enabled: Some(false),
            ..Default::default()
        })
        .unwrap();
        let out = tools.execute(&ToolCall {
            id: "1".into(),
            name: "web_search".into(),
            args_json: r#"{"query":"rust"}"#.into(),
        }).await;
        assert!(out.is_error);
        assert!(out.content.contains("turned off in Settings"), "{}", out.content);
        assert!(!tools.specs().iter().any(|s| s.name == "web_fetch"), "a tool that is off must not be offered");

        tools.set_web_enabled(true);
        let specs = tools.specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"web_search"));
        assert!(names.contains(&"web_fetch"));
    }

    #[test]
    fn adaptive_toolset_per_context_window() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            toolset_profile: Some(ToolsetProfile::Auto),
            web_enabled: Some(false),
            context_window: Some(32_000), // ~32k ultra-minimum -> compact
            ..Default::default()
        })
        .unwrap();
        let specs = tools.specs();
        assert_eq!(specs.len(), 10); // Compact toolset
        assert!(!specs.iter().any(|s| s.name == "patch_file"));

        tools.set_context_window(Some(65_536));
        let specs_large = tools.specs();
        assert_eq!(specs_large.len(), 18);
        assert!(specs_large.iter().any(|s| s.name == "patch_file"));
    }

    #[test]
    fn truncate_output_caps_and_marks() {
        let long = "x".repeat(MAX_OUTPUT_CHARS + 10);
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
        let edit: EditArgs = parse_args(r#"{"filePath":"a.rs","oldString":"x","newString":"y"}"#, "edit_file").unwrap();
        assert_eq!(edit.path, "a.rs");
        assert_eq!(edit.edits.len(), 1);
        assert_eq!((edit.edits[0].old_string.as_str(), edit.edits[0].new_string.as_str()), ("x", "y"));

        let shell: ShellArgs = parse_args(r#"{"cmd":"ls"}"#, "run_shell").unwrap();
        assert_eq!(shell.command.as_deref(), Some("ls"));

        let write: WriteArgs = parse_args(r#"{"file_path":"b.txt","contents":"hi"}"#, "write_file").unwrap();
        assert_eq!((write.path.as_str(), write.content.as_str()), ("b.txt", "hi"));
    }
}
