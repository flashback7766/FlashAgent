//! flashagent-tools: built-in toolset implementing [`flashagent_core::ToolExec`].
//!
//! Rich developer toolset over a fixed working directory: file read/write/edit/patch,
//! dir listing, glob and grep search, outline symbol inspection, git status/diff,
//! isolated shell execution, host environment detection, interactive user questioning (ask_user),
//! full project and global memory management, and web fetch/search.

pub mod ask_user;
pub mod env_tools;
pub mod fs_tools;
pub mod git;
pub mod mcp;
pub mod memory_tools;
pub mod outline;
pub mod patch;
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

pub use ask_user::QuestionGate;
pub use subagents::{agent_tools, BuiltinSubagentFactory, CompositeTools, ToolSubset};

/// Hard cap on tool result text fed back to the model.
const MAX_OUTPUT_CHARS: usize = 32_000;

/// Errors surfaced as tool results (`is_error`), never as panics.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Everything else: bad args, timeouts, HTTP failures.
    #[error("{0}")]
    Other(String),
}

/// Deserialize a call's arguments exactly as the permission layer and the
/// approval card read them (`flashagent_llm::effective_args`): one shape, no
/// fallback to a different part of the payload when the first read fails.
fn parse_args<T: DeserializeOwned>(json: &str, tool: &str) -> Result<T, ToolError> {
    let value = flashagent_llm::effective_args(json, tool)
        .ok_or_else(|| ToolError::Other(format!("bad arguments: not a JSON object: {json}")))?;
    serde_json::from_value(value).map_err(|e| ToolError::Other(format!("bad arguments: {e}")))
}

fn truncate_output(mut text: String) -> String {
    if text.chars().count() > MAX_OUTPUT_CHARS {
        text = text.chars().take(MAX_OUTPUT_CHARS).collect();
        text.push_str(&format!("\n...[truncated at {MAX_OUTPUT_CHARS} chars]"));
    }
    text
}

/// Construction options for [`BuiltinTools`].
pub struct BuiltinToolsConfig {
    /// Fixed working directory for all filesystem tools and the shell.
    pub cwd: PathBuf,
    /// Brave Search API key; without it `web_search` falls back to free DuckDuckGo.
    pub brave_api_key: Option<String>,
    /// Interactive question gate for `ask_user`.
    pub question_gate: Option<Arc<dyn QuestionGate>>,
    /// Shared atomic flag signaling autonomous `/goal` mode.
    pub is_goal_mode: Option<Arc<AtomicBool>>,
    /// Toolset exposure profile.
    pub toolset_profile: Option<ToolsetProfile>,
    /// Whether web fetch and search are enabled (PHILOSOPHY.md §3: opt-in only).
    pub web_enabled: Option<bool>,
    /// Discovered model context window size (PHILOSOPHY.md §9: adaptive toolset).
    pub context_window: Option<usize>,
    /// Optional MCP manager coordinating external Model Context Protocol servers.
    pub mcp_manager: Option<Arc<mcp::McpManager>>,
}

/// The built-in toolset. One instance per session, shared across the loop.
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
    mcp_manager: Arc<mcp::McpManager>,
}

impl BuiltinTools {
    /// Create a toolset rooted at `config.cwd`.
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
            web_enabled: Arc::new(AtomicBool::new(config.web_enabled.unwrap_or(false))),
            context_window: Arc::new(std::sync::RwLock::new(config.context_window)),
            mcp_manager,
        })
    }

    /// Return reference to the active MCP manager.
    pub fn mcp_manager(&self) -> Arc<mcp::McpManager> {
        self.mcp_manager.clone()
    }

    /// Set or update the active autonomous goal mode flag.
    pub fn set_goal_mode(&self, active: bool) {
        self.is_goal_mode.store(active, Ordering::Relaxed);
    }

    /// Return reference to the goal mode flag.
    pub fn goal_mode_flag(&self) -> Arc<AtomicBool> {
        self.is_goal_mode.clone()
    }

    /// Update toolset profile.
    pub fn set_toolset_profile(&self, profile: ToolsetProfile) {
        if let Ok(mut lock) = self.toolset_profile.write() {
            *lock = profile;
        }
    }

    /// Enable or disable web tools (PHILOSOPHY.md §3: opt-in only).
    pub fn set_web_enabled(&self, enabled: bool) {
        self.web_enabled.store(enabled, Ordering::Relaxed);
    }

    /// Update discovered context window size for adaptive toolset sizing (PHILOSOPHY.md §9).
    pub fn set_context_window(&self, window: Option<usize>) {
        if let Ok(mut lock) = self.context_window.write() {
            *lock = window;
        }
    }

    /// Unified diff of what a write/edit call would change, or `None` when the
    /// call is not a write or the file state cannot be read.
    fn preview_for(&self, call: &ToolCall) -> Option<String> {
        let v = flashagent_llm::effective_args(&call.args_json, &call.name)?;
        match call.name.as_str() {
            "write_file" => {
                let path = v.get("path")?.as_str()?;
                let new = v.get("content")?.as_str()?;
                let old = fs_tools::read_raw(&self.cwd, path);
                Some(flashagent_core::unified(old.as_deref(), new, path, 3))
            }
            "edit_file" => {
                let path = v.get("path")?.as_str()?;
                let edits: Vec<fs_tools::EditChunk> =
                    serde_json::from_value(v.get("edits")?.clone()).ok()?;
                let old = fs_tools::read_raw(&self.cwd, path)?;
                let new = fs_tools::apply_edits(old.clone(), &edits).ok()?;
                Some(flashagent_core::unified(Some(&old), &new, path, 3))
            }
            "patch_file" => {
                let path = v.get("path")?.as_str()?;
                let patch_text = v.get("patch")?.as_str()?;
                patch::preview_patch(&self.cwd, path, patch_text)
            }
            _ => None,
        }
    }

    async fn dispatch(&self, call: &ToolCall) -> Result<String, ToolError> {
        match call.name.as_str() {
            "read_file" => {
                let a: ReadArgs = parse_args(&call.args_json, &call.name)?;
                fs_tools::read_file(&self.cwd, &a.path, a.offset.unwrap_or(0), a.limit.unwrap_or(2000))
            }
            "write_file" => {
                let a: WriteArgs = parse_args(&call.args_json, &call.name)?;
                fs_tools::write_file(&self.cwd, &a.path, &a.content)
            }
            "edit_file" => {
                let a: EditArgs = parse_args(&call.args_json, &call.name)?;
                fs_tools::edit_file(&self.cwd, &a.path, &a.edits)
            }
            "patch_file" => {
                let a: PatchArgs = parse_args(&call.args_json, &call.name)?;
                patch::patch_file(&self.cwd, &a.path, &a.patch)
            }
            "list_dir" => {
                let a: ListArgs = parse_args(&call.args_json, &call.name)?;
                fs_tools::list_dir(&self.cwd, a.path.as_deref().unwrap_or("."))
            }
            "glob" => {
                let a: GlobArgs = parse_args(&call.args_json, &call.name)?;
                fs_tools::glob_files(&self.cwd, &a.pattern)
            }
            "grep" => {
                let a: GrepArgs = parse_args(&call.args_json, &call.name)?;
                fs_tools::grep(&self.cwd, &a.pattern, a.glob.as_deref(), a.case_insensitive)
            }
            "outline_file" => {
                let a: OutlineArgs = parse_args(&call.args_json, &call.name)?;
                outline::outline_file(&self.cwd, &a.path)
            }
            "git_status" => {
                let a: GitStatusArgs = parse_args(&call.args_json, &call.name)?;
                git::git_status(&self.cwd, a.path.as_deref())
            }
            "git_diff" => {
                let a: GitDiffArgs = parse_args(&call.args_json, &call.name)?;
                git::git_diff(&self.cwd, a.staged.unwrap_or(false), a.path.as_deref())
            }
            "run_shell" => {
                let a: ShellArgs = parse_args(&call.args_json, &call.name)?;
                self.run_shell(a).await
            }
            "env_info" => {
                env_tools::env_info(&self.cwd)
            }
            "ask_user" => {
                let a: ask_user::AskUserArgs = parse_args(&call.args_json, &call.name)?;
                ask_user::run_ask_user(self.question_gate.as_ref(), &self.is_goal_mode, a).await
            }
            "memory_read" => {
                let a: memory_tools::MemoryReadArgs = parse_args(&call.args_json, &call.name)?;
                memory_tools::memory_read(&self.cwd, a)
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
                    return Err(ToolError::Other("web_fetch is disabled by default per PHILOSOPHY.md §3 (local-first; web tools require explicit opt-in). Enable in settings/config.".into()));
                }
                let a: FetchArgs = parse_args(&call.args_json, &call.name)?;
                web::fetch_text(&self.http, &a.url).await
            }
            "web_search" => {
                if !self.web_enabled.load(Ordering::Relaxed) {
                    return Err(ToolError::Other("web_search is disabled by default per PHILOSOPHY.md §3 (local-first; web tools require explicit opt-in). Enable in settings/config.".into()));
                }
                let a: SearchArgs = parse_args(&call.args_json, &call.name)?;
                if let Some(ref key) = self.brave_key {
                    if !key.is_empty() {
                        web::search(&self.http, key, &a.query, a.count.unwrap_or(5)).await
                    } else {
                        web::search_free(&self.http, &a.query, a.count.unwrap_or(5)).await
                    }
                } else {
                    web::search_free(&self.http, &a.query, a.count.unwrap_or(5)).await
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
}

#[async_trait]
impl ToolExec for BuiltinTools {
    async fn execute(&self, call: &ToolCall) -> ToolOutput {
        match self.dispatch(call).await {
            Ok(text) => ToolOutput { content: truncate_output(text), is_error: false },
            Err(e) => ToolOutput { content: format!("error: {e}"), is_error: true },
        }
    }

    fn specs(&self) -> Vec<ToolSpec> {
        // Core tools (present in all profiles)
        let mut specs = vec![
            ToolSpec {
                name: "read_file".into(),
                description: "Read a text file with 1-based line numbers".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "path": {"type": "string", "description": "File path, relative to working directory or absolute"}, "offset": {"type": "integer", "description": "0-based first line to read"}, "limit": {"type": "integer", "description": "Max lines to read (default 2000)"}}, "required": ["header", "path"]}"#.into(),
            },
            ToolSpec {
                name: "write_file".into(),
                description: "Create or fully overwrite a file".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "path": {"type": "string"}, "content": {"type": "string"}}, "required": ["header", "path", "content"]}"#.into(),
            },
            ToolSpec {
                name: "edit_file".into(),
                description: "Apply surgical string replacements to a file; each edit replaces exact old_string matches".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "path": {"type": "string"}, "edits": {"type": "array", "items": {"type": "object", "properties": {"old_string": {"type": "string"}, "new_string": {"type": "string"}, "replace_all": {"type": "boolean"}}, "required": ["old_string", "new_string"]}}}, "required": ["header", "path", "edits"]}"#.into(),
            },
            ToolSpec {
                name: "list_dir".into(),
                description: "List a directory; directory entries end with /".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "path": {"type": "string", "description": "Defaults to the working directory"}}, "required": ["header"]}"#.into(),
            },
            ToolSpec {
                name: "glob".into(),
                description: "Find files by glob pattern (e.g. src/**/*.rs), max 500 results".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "pattern": {"type": "string"}}, "required": ["header", "pattern"]}"#.into(),
            },
            ToolSpec {
                name: "grep".into(),
                description: "Search file contents by regex; returns path:line:text".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "pattern": {"type": "string"}, "glob": {"type": "string", "description": "Restrict to files matching this glob"}, "case_insensitive": {"type": "boolean"}}, "required": ["header", "pattern"]}"#.into(),
            },
            ToolSpec {
                name: "run_shell".into(),
                description: "Run a shell command. Foreground by default (timeout_ms, default 120000). With background:true returns a task id; poll with {\"task_id\":N}, terminate with {\"task_id\":N,\"kill\":true}".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "command": {"type": "string"}, "background": {"type": "boolean"}, "timeout_ms": {"type": "integer"}, "task_id": {"type": "integer", "description": "Poll or kill a background task"}, "kill": {"type": "boolean", "description": "With task_id: kill the task"}}, "required": ["header"]}"#.into(),
            },
            ToolSpec {
                name: "ask_user".into(),
                description: "Ask the user 1 or more questions to obtain guidance, clarification, or choices. ALWAYS use this tool whenever you need choices or clarification (topic, language, scope, format). Provide selectable options (up to 10 choices) or leave options empty for open-ended text input".into(),
                parameters_json: r#"{"type": "object", "properties": {"question": {"type": "string", "description": "Single question to ask the user"}, "options": {"type": "array", "items": {"type": "string"}, "description": "Optional list of selectable options (up to 10)"}, "multi_select": {"type": "boolean", "description": "Allow user to select multiple options simultaneously"}, "questions": {"type": "array", "items": {"type": "object", "properties": {"question": {"type": "string"}, "options": {"type": "array", "items": {"type": "string"}}, "multi_select": {"type": "boolean"}}, "required": ["question"]}, "description": "List of multiple questions to ask in sequence"}}, "required": []}"#.into(),
            },
            ToolSpec {
                name: "memory_read".into(),
                description: "List what is remembered, or read one memory in full by name. The index of every memory is already in your context; use this to read a body.".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "scope": {"type": "string", "enum": ["project", "global", "all"], "description": "Which memory to look at; 'all' by default."}, "name": {"type": "string", "description": "Name of one memory to read in full. Omit to list what is remembered."}}, "required": ["header"]}"#.into(),
            },
        ];

        // Extended tools (Auto and Full profiles, adapted by context window per PHILOSOPHY.md §9)
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
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "path": {"type": "string", "description": "File path to patch"}, "patch": {"type": "string", "description": "Unified diff patch text with @@ hunks"}}, "required": ["header", "path", "patch"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "outline_file".into(),
                description: "Extract structural outline of a file (functions, structs, classes, traits, headings) with line numbers without reading the entire content".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "path": {"type": "string", "description": "File path to inspect"}}, "required": ["header", "path"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "git_status".into(),
                description: "Inspect git repository status: current branch, staged, unstaged, and untracked files".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "path": {"type": "string", "description": "Optional subpath filter"}}, "required": ["header"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "git_diff".into(),
                description: "View unified diff of working tree changes or staged changes".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "staged": {"type": "boolean", "description": "View staged/cached diff if true"}, "path": {"type": "string", "description": "Optional file path filter"}}, "required": ["header"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "env_info".into(),
                description: "Inspect OS platform, CPU architecture, working directory, and installed developer toolchain versions".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}}, "required": ["header"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "memory_create".into(),
                description: "Remember one fact across sessions: something the user told you about how they work, or a decision about this project and its reason. Not things the code, git history or docs already say. Disabled in autonomous /goal mode".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "title": {"type": "string", "description": "Short title; it becomes the memory's name."}, "content": {"type": "string", "description": "The fact itself, in full sentences, with the reason behind it when there is one."}, "description": {"type": "string", "description": "One line saying what this memory is about. It goes in the index that is loaded every turn, so make it specific."}, "type": {"type": "string", "enum": ["preference", "decision", "reference", "work"], "description": "preference = how the user likes to work; decision = a choice made about this project and why; reference = a pointer outwards (URL, ticket); work = ongoing goals or constraints."}, "scope": {"type": "string", "enum": ["project", "global"], "description": "Use 'global' for anything about the USER — how they work, what they prefer, corrections they gave you — so it follows them into every project. Use 'project' only for facts about this codebase. A sentence that starts with 'I' or 'the user' is global."}}, "required": ["header", "title", "content"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "memory_update".into(),
                description: "Correct something already remembered, when it turns out to be wrong or has changed. Disabled in autonomous /goal mode".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "title": {"type": "string", "description": "Name of the memory to correct."}, "content": {"type": "string", "description": "The corrected fact."}, "description": {"type": "string", "description": "One line saying what this memory is about. It goes in the index that is loaded every turn, so make it specific."}, "type": {"type": "string", "enum": ["preference", "decision", "reference", "work"], "description": "preference = how the user likes to work; decision = a choice made about this project and why; reference = a pointer outwards (URL, ticket); work = ongoing goals or constraints."}, "scope": {"type": "string", "enum": ["project", "global"], "description": "Use 'global' for anything about the USER — how they work, what they prefer, corrections they gave you — so it follows them into every project. Use 'project' only for facts about this codebase. A sentence that starts with 'I' or 'the user' is global."}}, "required": ["header", "title", "content"]}"#.into(),
            });
            specs.push(ToolSpec {
                name: "memory_remove".into(),
                description: "Forget a memory that turned out to be wrong or no longer applies. Disabled in autonomous /goal mode".into(),
                parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "title": {"type": "string", "description": "Name of the memory to forget."}, "scope": {"type": "string", "enum": ["project", "global"], "description": "Use 'global' for anything about the USER — how they work, what they prefer, corrections they gave you — so it follows them into every project. Use 'project' only for facts about this codebase. A sentence that starts with 'I' or 'the user' is global."}}, "required": ["header", "title"]}"#.into(),
            });
            // PHILOSOPHY.md §3: Web tools require explicit opt-in
            if self.web_enabled.load(Ordering::Relaxed) {
                specs.push(ToolSpec {
                    name: "web_fetch".into(),
                    description: "Fetch a URL as text; HTML is reduced to plain text".into(),
                    parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "url": {"type": "string"}}, "required": ["header", "url"]}"#.into(),
                });
                specs.push(ToolSpec {
                    name: "web_search".into(),
                    description: "Search the web for documentation, articles, and solutions".into(),
                    parameters_json: r#"{"type": "object", "properties": {"header": {"type": "string", "description": "One short line, in the user's language, saying what this call is for — e.g. 'Read the loop that applies the patch'. The user sees it instead of the raw call, so write one for every call, including reads and searches."}, "query": {"type": "string"}, "count": {"type": "integer", "description": "Results to return (default 5)"}}, "required": ["header", "query"]}"#.into(),
                });
            }
        }

        // MCP tools (dynamically discovered from active MCP servers)
        specs.extend(self.mcp_manager.get_all_tool_specs());

        specs
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[derive(Deserialize)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct WriteArgs {
    path: String,
    content: String,
}

#[derive(Deserialize)]
struct EditArgs {
    path: String,
    edits: Vec<fs_tools::EditChunk>,
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
    fn edit_file_approval_card_gets_a_diff() {
        // The approval card promises a diff before anything is written; if the
        // preview returns None the user approves blind.
        let dir = std::env::temp_dir().join(format!("fa-preview-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/parser.rs"), "fn a() {}\npub fn parse_duration() {}\n").unwrap();

        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir.clone(),
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: None,
            web_enabled: None,
            context_window: None,
            mcp_manager: None,
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

    use super::*;

    #[tokio::test]
    async fn hostile_args_become_errors_not_panics() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: None,
            web_enabled: None,
            context_window: None,
            mcp_manager: None,
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

    #[tokio::test]
    async fn missing_file_is_tool_error() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: None,
            web_enabled: None,
            context_window: None,
            mcp_manager: None,
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
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: Some(ToolsetProfile::Auto),
            web_enabled: None,
            context_window: None,
            mcp_manager: None,
        })
        .unwrap();
        let specs_auto = tools_auto.specs();
        assert_eq!(specs_auto.len(), 17); // 9 core + 8 extended, web disabled by default per PHILOSOPHY.md §3
        let names_auto: Vec<&str> = specs_auto.iter().map(|s| s.name.as_str()).collect();
        assert!(names_auto.contains(&"ask_user"));
        assert!(names_auto.contains(&"outline_file"));
        assert!(names_auto.contains(&"git_status"));
        assert!(names_auto.contains(&"git_diff"));
        assert!(names_auto.contains(&"patch_file"));
        assert!(names_auto.contains(&"env_info"));
        assert!(names_auto.contains(&"memory_read"));
        assert!(names_auto.contains(&"memory_create"));
        assert!(!names_auto.contains(&"web_search")); // opt-in only

        let tools_compact = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: Some(ToolsetProfile::Compact),
            web_enabled: None,
            context_window: None,
            mcp_manager: None,
        })
        .unwrap();
        let specs_compact = tools_compact.specs();
        assert_eq!(specs_compact.len(), 9);
        let names_compact: Vec<&str> = specs_compact.iter().map(|s| s.name.as_str()).collect();
        assert!(names_compact.contains(&"ask_user"));
        assert!(!names_compact.contains(&"patch_file"));
    }

    #[tokio::test]
    async fn web_tools_require_opt_in_per_philosophy() {
        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: testing::tempdir(),
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: Some(ToolsetProfile::Auto),
            web_enabled: Some(false),
            context_window: None,
            mcp_manager: None,
        })
        .unwrap();
        let out = tools.execute(&ToolCall {
            id: "1".into(),
            name: "web_search".into(),
            args_json: r#"{"query":"rust"}"#.into(),
        }).await;
        assert!(out.is_error);
        assert!(out.content.contains("PHILOSOPHY.md §3"));

        // When opt-in enabled:
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
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: Some(ToolsetProfile::Auto),
            web_enabled: Some(false),
            context_window: Some(32_000), // ~32k ultra-minimum -> compact
            mcp_manager: None,
        })
        .unwrap();
        let specs = tools.specs();
        assert_eq!(specs.len(), 9); // Compact toolset
        assert!(!specs.iter().any(|s| s.name == "patch_file"));

        // On larger window (~65k+):
        tools.set_context_window(Some(65_536));
        let specs_large = tools.specs();
        assert_eq!(specs_large.len(), 17);
        assert!(specs_large.iter().any(|s| s.name == "patch_file"));
    }

    #[test]
    fn truncate_output_caps_and_marks() {
        let long = "x".repeat(MAX_OUTPUT_CHARS + 10);
        let out = truncate_output(long);
        assert!(out.contains("[truncated"));
        assert_eq!(out.chars().count(), MAX_OUTPUT_CHARS + "\n...[truncated at 32000 chars]".len());
    }

    #[test]
    fn parse_args_self_healing_python_dict_and_unwrapping() {
        // Test Python dict with single quotes
        let py_dict = "{'path': 'src/main.rs', 'offset': 10}";
        let read: ReadArgs = parse_args(py_dict, "read_file").unwrap();
        assert_eq!(read.path, "src/main.rs");
        assert_eq!(read.offset, Some(10));

        // Test wrapped in {"arguments": {...}}
        let wrapped = r#"{"arguments": {"path": "src/lib.rs", "offset": 5}}"#;
        let read2: ReadArgs = parse_args(wrapped, "read_file").unwrap();
        assert_eq!(read2.path, "src/lib.rs");
        assert_eq!(read2.offset, Some(5));

        // Test single-key wrapper {"read_file": {"path": "README.md"}}
        let single_key = r#"{"read_file": {"path": "README.md"}}"#;
        let read3: ReadArgs = parse_args(single_key, "read_file").unwrap();
        assert_eq!(read3.path, "README.md");
    }
}
