//! flashagent-core: the agent loop.
//!
//! The loop consumes two traits — [`LlmSource`] (streamed model turns) and
//! [`ToolExec`] (tool execution) — and knows nothing about HTTP, SQL or UI.
//! Everything the user sees flows out as [`LoopEvent`]s.

pub mod config;
pub mod context_usage;
pub mod diff;
pub mod effort_memory;
pub mod loop_;
pub mod memory;
pub mod memory_store;
pub mod permissions;
pub mod personality;
pub mod prompt;
pub mod snapshots;
pub mod subagents;
pub mod toolcheck;

pub use config::{AppConfig, BackendPreset, SamplingPreset, ToolsetProfile, UpdateChannel};
pub use context_usage::{
    default_compact_threshold, resolved_compact_threshold, should_compact, CompactionInput,
    CompactionVerdict, ContextUsage,
};
pub use diff::unified;
pub use flashagent_llm::base64_encode;
pub use effort_memory::{EffortMemory, ModelBias, TurnOutcome};
pub use loop_::{AgentLoop, DoneReason, LoopConfig, LoopError, LoopEvent, LlmSource, ToolExec, ToolOutput, WritePreview};
pub use memory_store::{Entry as MemoryEntry, Kind as MemoryEntryKind, Scope as MemoryScope, Store as MemoryStore};
pub use memory::{collect, injection_block, outline, MemoryDoc, MemoryKind, GLOBAL_FILENAME, PROJECT_FILENAMES};
pub use permissions::{
    is_local_host, is_local_ip, is_read_only_shell, parse_chain, path_is_inside, url_host, ApprovalGate,
    ApprovalRequest, Category, Decision, DenyAllGate, PermissionMode, PermissionState, PermissionedTools, RuleSet,
    Verdict,
};
pub use personality::{BaseStyle, Level as TraitLevel, Personality, Trait as PersonalityTrait};
pub use prompt::{build_system_prompt, SystemPromptConfig};
pub use snapshots::{FilePreview, RewindReport, Rewindable, SnapshotStore};
pub use subagents::{
    AgentRole, SubagentHandle, SubagentHost, SubagentMsg, SubagentResult, SubagentSpec,
    SubagentTool, SubagentToolFactory,
};
