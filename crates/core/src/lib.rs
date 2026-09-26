//! The agent loop. It consumes [`LlmSource`] and [`ToolExec`], knows nothing
//! about HTTP or UI, and reports everything as [`LoopEvent`]s.

pub mod config;
pub mod context_usage;
pub mod diff;
pub mod effort_memory;
pub mod loop_;
pub mod memory;
pub mod memory_store;
pub mod paths;
pub mod permissions;
pub mod personality;
pub mod prompt;
mod repetition;
pub mod snapshots;
pub mod subagents;
pub mod task_notices;
pub mod toolcheck;

pub use config::{AppConfig, BackendPreset, ColorTheme, SamplingPreset, ToolsetProfile, UpdateChannel};
pub use context_usage::{
    default_compact_threshold, resolved_compact_threshold, should_compact, CompactionInput,
    CompactionVerdict, ContextUsage,
};
pub use diff::unified;
pub use flashagent_llm::base64_encode;
pub use effort_memory::{EffortMemory, ModelBias, TurnOutcome};
pub use loop_::{is_prompt, AgentLoop, CANCELLED_RESULT, DoneReason, LoopConfig, LoopError, LoopEvent, LlmSource, ToolExec, ToolOutput, WritePreview};
pub use memory_store::{Entry as MemoryEntry, Kind as MemoryEntryKind, Scope as MemoryScope, Store as MemoryStore};
pub use memory::{collect, injection_block, outline, MemoryDoc, MemoryKind, GLOBAL_FILENAME};
pub use paths::{expand_home, resolve_path};
pub use permissions::{
    is_local_host, is_local_ip, is_read_only_shell, parse_chain, path_is_inside, set_shell_dialect, url_host,
    ApprovalGate, ApprovalRequest, Category, Decision, DenyAllGate, PermissionMode, PermissionState,
    PermissionedTools, RuleSet, ShellDialect, Verdict,
};
pub use personality::{BaseStyle, Level as TraitLevel, Personality, Trait as PersonalityTrait};
pub use prompt::{build_system_prompt, SystemPromptConfig};
pub use task_notices::{is_task_notice, NoticeInbox, TASK_NOTICE_NOTE, TASK_NOTICE_OPENING};
pub use snapshots::{FilePreview, RewindReport, Rewindable, SnapshotStore};
pub use subagents::{
    AgentRole, SubagentHandle, SubagentHost, SubagentResult, SubagentSpec, SubagentTool,
    SubagentToolFactory,
};
