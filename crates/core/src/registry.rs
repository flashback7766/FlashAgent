//! Every built-in tool, declared once.
//!
//! A tool used to live in four places that had to be kept in step by hand: the
//! JSON schema the model reads, the dispatcher that runs it, the diff the
//! approval card shows, and the permission layer's own `match` on its name.
//! Adding a tool meant editing all four and trusting that you had not missed
//! one. The registry is the one declaration, and the other three are read from
//! it: a name nobody wired up fails to compile, and a name nobody tests is
//! caught by a test here rather than by a user.
//!
//! What stays out of it, on purpose: MCP tools, whose names and schemas a
//! server chooses, and `spawn_agent`'s implementation, which lives in
//! `subagents.rs` next to the loop it runs.

use crate::permissions::Category;

/// One parameter of a tool, as data rather than as a line of JSON.
#[derive(Debug, Clone, Copy)]
pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    pub description: &'static str,
    /// The only values a model may send, when there are any.
    pub choices: &'static [&'static str],
    /// May be left out. Inside a list of objects this is what keeps a model
    /// from sending `replace_all: false` on every edit because the schema told
    /// it the field was required.
    pub optional: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Str,
    Int,
    Bool,
    /// A list of plain strings.
    Strings,
    /// A list whose every entry is an object with these properties.
    Objects(&'static [Param]),
}

const fn s(name: &'static str, description: &'static str) -> Param {
    Param { name, kind: Kind::Str, description, choices: &[], optional: false }
}

const fn int(name: &'static str, description: &'static str) -> Param {
    Param { name, kind: Kind::Int, description, choices: &[], optional: false }
}

const fn boolean(name: &'static str, description: &'static str) -> Param {
    Param { name, kind: Kind::Bool, description, choices: &[], optional: false }
}

const fn list(name: &'static str, description: &'static str, items: &'static [Param]) -> Param {
    Param { name, kind: Kind::Objects(items), description, choices: &[], optional: false }
}

/// A list of plain strings. Whether it is required is said where it is used:
/// the schema of a tool spells out its own top-level `required`, and inside a
/// list it is required unless marked optional.
const fn strings(name: &'static str, description: &'static str) -> Param {
    Param { name, kind: Kind::Strings, description, choices: &[], optional: false }
}

const fn opt_int(name: &'static str, description: &'static str) -> Param {
    Param { name, kind: Kind::Int, description, choices: &[], optional: true }
}

const fn opt_bool(name: &'static str, description: &'static str) -> Param {
    Param { name, kind: Kind::Bool, description, choices: &[], optional: true }
}

const fn one_of(name: &'static str, description: &'static str, choices: &'static [&'static str]) -> Param {
    Param { name, kind: Kind::Str, description, choices, optional: false }
}

/// The note that used to be copied into every single schema, 270 characters
/// each time, and once grew the whole system prompt by a third.
const HEADER: Param = s("header", "What this call is for, one short line in the user's language. Shown to the user instead of the call.");

/// Which arguments name files on disk, so the permission layer can ask whether a
/// call reaches outside the project without knowing any tool names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Touches {
    /// `path`, plus `files[].path` when several are named at once.
    Files,
    /// A glob: the fixed part before the first wildcard is what it searches from.
    Glob,
    /// Nothing on disk, so the call cannot leave the project.
    Nothing,
}

/// What the approval card shows before the call runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    /// A diff of one file written whole.
    Write,
    /// A diff of exact-match edits, over one file or twenty.
    Edit,
    /// A diff of a patch applied to one file.
    Patch,
    /// Not a write: the card shows the call and no diff.
    None,
}

/// Which code runs the call. The tools crate matches on this, so a tool added
/// here without a handler anywhere is a compile error rather than a call that
/// answers `unknown tool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handler {
    ReadFile,
    WriteFile,
    EditFile,
    PatchFile,
    ListDir,
    Glob,
    Grep,
    OutlineFile,
    GitStatus,
    GitDiff,
    RunShell,
    EnvInfo,
    ViewImage,
    AskUser,
    UpdatePlan,
    MemoryRead,
    MemoryCreate,
    MemoryUpdate,
    MemoryRemove,
    WebFetch,
    WebSearch,
    SpawnAgent,
    SendMessage,
    ReviewAgent,
}

impl Handler {
    /// The tool this handler belongs to. The registry test holds the two
    /// together, so a renamed tool cannot keep an old handler.
    pub fn tool_name(self) -> &'static str {
        match self {
            Handler::ReadFile => "read_file",
            Handler::WriteFile => "write_file",
            Handler::EditFile => "edit_file",
            Handler::PatchFile => "patch_file",
            Handler::ListDir => "list_dir",
            Handler::Glob => "glob",
            Handler::Grep => "grep",
            Handler::OutlineFile => "outline_file",
            Handler::GitStatus => "git_status",
            Handler::GitDiff => "git_diff",
            Handler::RunShell => "run_shell",
            Handler::EnvInfo => "env_info",
            Handler::ViewImage => "view_image",
            Handler::AskUser => "ask_user",
            Handler::UpdatePlan => "update_plan",
            Handler::MemoryRead => "memory_read",
            Handler::MemoryCreate => "memory_create",
            Handler::MemoryUpdate => "memory_update",
            Handler::MemoryRemove => "memory_remove",
            Handler::WebFetch => "web_fetch",
            Handler::WebSearch => "web_search",
            Handler::SpawnAgent => "spawn_agent",
            Handler::SendMessage => "send_message",
            Handler::ReviewAgent => "review_agent",
        }
    }
}

/// When the model is offered the tool. `Always` is the plain case; the rest are
/// offered only when what the server said, or Settings, says they can work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// Every conversation, whatever the window and whatever the model can do.
    Always,
    /// Only when the tool schemas are worth their tokens: a small window gets
    /// the core set.
    Core,
    /// Only when the server said the model can see images.
    Vision,
    /// Only while web tools are on in Settings.
    Web,
    /// Only the conversation at the top: a subagent does not review the work
    /// that sent it, and a tool it can never call is a tool it will ask for.
    Parent,
}

pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// Everything this call takes, apart from the note in `HEADER`, which is
    /// added for every tool that does not say otherwise.
    pub params: &'static [Param],
    /// What must be there. The header is required for every tool that has one.
    pub required: &'static [&'static str],
    /// `false` for the two tools that are not narrated: asking a question, and
    /// writing down a plan the model already showed the user.
    pub has_header: bool,
    pub handler: Handler,
    pub category: Category,
    pub touches: Touches,
    pub preview: Preview,
    pub availability: Availability,
}

const PATH: Param = s("path", "File path, relative to the working directory or absolute");

/// `read_file` and its siblings take a list too, so twenty files are one call.
const FILES_ITEM: &[Param] = &[
    PATH,
    opt_int("offset", "0-based first line to read"),
    opt_int("limit", "Max lines to read"),
];

const FILES: Param = list(
    "files",
    "Several files in one call, each read like path/offset/limit",
    FILES_ITEM,
);

const EDITS_ITEM: &[Param] = &[
    s(
        "old_string",
        "Text to replace, copied exactly from the file with its indentation and line breaks; it must match once unless replace_all",
    ),
    s("new_string", "The text that replaces old_string whole"),
    opt_bool("replace_all", "Replace every match, not just one"),
];

const EDITS: Param = list("edits", "Replacements, applied in order", EDITS_ITEM);

const MEMORY_TYPE: Param = one_of(
    "type",
    "preference = how the user likes to work; decision = a choice made about this project and why; reference = a pointer outwards (URL, ticket); work = ongoing goals or constraints.",
    &["preference", "decision", "reference", "work"],
);

const MEMORY_SCOPE: Param = one_of(
    "scope",
    "Use 'global' for anything about the USER — how they work, what they prefer, corrections they gave you — so it follows them into every project. Use 'project' only for facts about this codebase. A sentence that starts with 'I' or 'the user' is global.",
    &["project", "global"],
);

const MEMORY_DESCRIPTION: Param =
    s("description", "One line saying what this memory is about. It goes in the index that is loaded every turn, so make it specific.");

const WRITE_MEMORY: &[Param] = &[
    s("title", "Short title; it becomes the memory's name."),
    s("content", "The fact itself, in full sentences, with the reason behind it when there is one."),
    MEMORY_DESCRIPTION,
    MEMORY_TYPE,
    MEMORY_SCOPE,
];

/// The built-in toolset, in the order the model reads it. `spawn_agent` is last
/// because it is the one that costs the most when called by mistake.
pub static TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "read_file",
        description: "Read a text file with 1-based line numbers. For several files pass files instead of path (up to 20)",
        params: &[PATH, int("offset", "0-based first line to read"), int("limit", "Max lines to read (default 2000)"), FILES],
        required: &[],
        has_header: true,
        handler: Handler::ReadFile,
        category: Category::Read,
        touches: Touches::Files,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "write_file",
        description: "Create or fully overwrite a file",
        params: &[PATH, s("content", "The whole new text of the file, exactly as it must be, every line break and quote included")],
        required: &["path", "content"],
        has_header: true,
        handler: Handler::WriteFile,
        category: Category::Write,
        touches: Touches::Files,
        preview: Preview::Write,
        availability: Availability::Always,
    },
    ToolDef {
        name: "edit_file",
        description: "Replace exact old_string matches in a file; new_string replaces old_string whole, so to insert a line keep its neighbour in new_string too. For several files pass files instead of path and edits (up to 20); all apply or none do",
        params: &[
            PATH,
            EDITS,
            list(
                "files",
                "Several files changed as one change",
                &[PATH, EDITS],
            ),
        ],
        required: &["path", "edits"],
        has_header: true,
        handler: Handler::EditFile,
        category: Category::Write,
        touches: Touches::Files,
        preview: Preview::Edit,
        availability: Availability::Always,
    },
    ToolDef {
        name: "list_dir",
        description: "List a directory; directory entries end with /",
        params: &[s("path", "Defaults to the working directory")],
        required: &[],
        has_header: true,
        handler: Handler::ListDir,
        category: Category::Read,
        touches: Touches::Files,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "glob",
        description: "Find files by glob pattern (e.g. src/**/*.rs), max 500 results",
        params: &[s("pattern", "Glob pattern, relative to the working directory")],
        required: &["pattern"],
        has_header: true,
        handler: Handler::Glob,
        category: Category::Read,
        touches: Touches::Glob,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "grep",
        description: "Search file contents by regex; returns path:line:text",
        params: &[
            s("pattern", "Regular expression to search for"),
            s("glob", "Restrict to files matching this glob"),
            boolean("case_insensitive", "Match regardless of case"),
        ],
        required: &["pattern"],
        has_header: true,
        handler: Handler::Grep,
        category: Category::Read,
        // A search reads files the model did not name, so what it reaches is
        // the whole tree: the boundary is checked by the category, not by path.
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "run_shell",
        description: "Run a shell command (timeout_ms, default 120000). background:true for servers, watchers and long builds: returns a task_id at once, and a notice arrives when the task exits, so do not poll in a loop. task_id alone shows its output so far; with kill:true stops it",
        params: &[
            s("command", "The command line to run in the working directory"),            s("content", "The whole new text of the file, exactly as it must be, every line break and quote included"),
            opt_bool("background", "Run it in the background and return a task_id at once"),
            opt_int("timeout_ms", "Kill it after this many milliseconds (default 120000)"),
            opt_int("task_id", "A background task: its output so far, or with kill stop it"),
            opt_bool("kill", "With task_id: kill the task"),
        ],
        required: &[],
        has_header: true,
        handler: Handler::RunShell,
        category: Category::Shell,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "ask_user",
        description: "The only way to ask the user anything. Give every question 2-6 short, concrete answers to pick from as options (answers, not more questions); the user can still type their own. Put several questions in questions",
        params: &[
            s("question", "Single question to ask the user"),
            strings("options", "2-6 answers to pick from"),
            opt_bool("multi_select", "Allow picking several options"),
            list(
                "questions",
                "Several questions, asked in order",
                &[s("question", "One question"), strings("options", "2-6 answers to pick from"), opt_bool("multi_select", "Allow picking several options")],
            ),
        ],
        required: &[],
        has_header: false,
        handler: Handler::AskUser,
        category: Category::Read,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "memory_read",
        description: "Read one remembered fact in full by name, or list them. The index is already in your context.",
        params: &[
            one_of("scope", "Which memory to look at; 'all' by default.", &["project", "global", "all"]),
            s("name", "Name of one memory to read in full. Omit to list what is remembered."),
        ],
        required: &[],
        has_header: true,
        handler: Handler::MemoryRead,
        category: Category::Read,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "update_plan",
        // Offered always, though it refuses itself outside /goal. The tool list
        // is part of the cached prompt: adding it when a goal started made the
        // first goal step re-read the whole conversation.
        description: "Only during /goal: record the whole step-by-step plan, again each time a step finishes or the plan changes. Does nothing outside /goal.",
        params: &[list(
            "steps",
            "The whole plan, in order",
            &[
                s("text", "One step, in a few words"),
                Param {
                    name: "status",
                    kind: Kind::Str,
                    description: "Where the step stands",
                    choices: &["pending", "in_progress", "completed"],
                    optional: true,
                },
            ],
        )],
        required: &["steps"],
        has_header: false,
        handler: Handler::UpdatePlan,
        category: Category::Read,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "patch_file",
        description: "Apply a standard unified diff patch to a target file",
        params: &[s("path", "File path to patch"), s("patch", "Unified diff patch text with @@ hunks")],
        required: &["path", "patch"],
        has_header: true,
        handler: Handler::PatchFile,
        category: Category::Write,
        touches: Touches::Files,
        preview: Preview::Patch,
        availability: Availability::Core,
    },
    ToolDef {
        name: "outline_file",
        description: "Extract structural outline of a file (functions, structs, classes, traits, headings) with line numbers without reading the entire content",
        params: &[s("path", "File path to inspect")],
        required: &["path"],
        has_header: true,
        handler: Handler::OutlineFile,
        category: Category::Read,
        touches: Touches::Files,
        preview: Preview::None,
        availability: Availability::Core,
    },
    ToolDef {
        name: "git_status",
        description: "Inspect git repository status: current branch, staged, unstaged, and untracked files",
        params: &[s("path", "Optional subpath filter")],
        required: &[],
        has_header: true,
        handler: Handler::GitStatus,
        category: Category::Read,
        touches: Touches::Files,
        preview: Preview::None,
        availability: Availability::Core,
    },
    ToolDef {
        name: "git_diff",
        description: "View unified diff of working tree changes or staged changes",
        params: &[boolean("staged", "View staged/cached diff if true"), s("path", "Optional file path filter")],
        required: &[],
        has_header: true,
        handler: Handler::GitDiff,
        category: Category::Read,
        touches: Touches::Files,
        preview: Preview::None,
        availability: Availability::Core,
    },
    ToolDef {
        name: "env_info",
        description: "Inspect OS platform, CPU architecture, working directory, and installed developer toolchain versions",
        params: &[],
        required: &[],
        has_header: true,
        handler: Handler::EnvInfo,
        category: Category::Read,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Core,
    },
    ToolDef {
        name: "memory_create",
        description: "Remember one fact across sessions: something the user told you about how they work, or a decision about this project and its reason. Not things the code, git history or docs already say. Disabled in autonomous /goal mode",
        params: WRITE_MEMORY,
        required: &["title", "content"],
        has_header: true,
        handler: Handler::MemoryCreate,
        category: Category::Write,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Core,
    },
    ToolDef {
        name: "memory_update",
        description: "Correct something already remembered, when it turns out to be wrong or has changed. Disabled in autonomous /goal mode",
        params: WRITE_MEMORY,
        required: &["title", "content"],
        has_header: true,
        handler: Handler::MemoryUpdate,
        category: Category::Write,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Core,
    },
    ToolDef {
        name: "memory_remove",
        description: "Forget a memory that turned out to be wrong or no longer applies. Disabled in autonomous /goal mode",
        params: &[
            s("title", "Name of the memory to forget."),
            MEMORY_SCOPE,
        ],
        required: &["title"],
        has_header: true,
        handler: Handler::MemoryRemove,
        category: Category::Write,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Core,
    },
    ToolDef {
        name: "view_image",
        description: "Look at an image in the project — a diagram, a screenshot, a mockup. The picture itself comes back, so describe what you see rather than guessing from the file name.",
        params: &[s("path", "Path to the image inside the project (png, jpg, gif, webp, bmp).")],
        required: &["path"],
        has_header: true,
        handler: Handler::ViewImage,
        category: Category::Read,
        touches: Touches::Files,
        preview: Preview::None,
        availability: Availability::Vision,
    },
    ToolDef {
        name: "web_fetch",
        description: "Fetch a URL as text; HTML is reduced to plain text",
        params: &[s("url", "Full URL, starting with http:// or https://")],
        required: &["url"],
        has_header: true,
        handler: Handler::WebFetch,
        category: Category::Net,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Web,
    },
    ToolDef {
        name: "web_search",
        description: "Search the web for documentation, articles, and solutions",
        params: &[s("query", "What to search for"), int("count", "Results to return (default 5)")],
        required: &["query"],
        has_header: true,
        handler: Handler::WebSearch,
        category: Category::Net,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Web,
    },
    ToolDef {
        name: "spawn_agent",
        description: "Start a subagent on a self-contained task and carry on without waiting for it. Returns an agent_id at once, and the answer arrives on its own; as many may run at once as the work needs, and a researcher or reviewer cannot write files or run commands.",
        params: &[
            one_of(
                "role",
                "researcher (reads and searches, cannot write), coder (writes and runs tests), reviewer (reads, reports problems), planner (reads, returns a plan), or a custom role",
                &["researcher", "coder", "reviewer", "planner"],
            ),
            s("task", "The whole task, self-contained: say what to do, where, and what to report back"),
            int("max_steps", "Max loop steps (default 500; the last one asks the subagent to wrap up and report rather than cutting it off)"),
            int("timeout_secs", "Max wall-clock seconds (default none)"),
        ],
        required: &["task"],
        has_header: false,
        handler: Handler::SpawnAgent,
        // Every call a child makes goes through the parent's permission state,
        // so starting one is a read: what it may then do is the role's boundary.
        category: Category::Read,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "send_message",
        description: "Send a message to another agent working on this task, or to the parent with to \"parent\". Use it to hand over a finding, ask a running sibling a question, or redirect one of your own children. A message reaches the other agent between its steps, the way a keystroke from the user would; it does not stop it. The other agent decides what to do with it.",
        params: &[
            one_of("to", "The agent_id of the agent to write to, or \"parent\" for the one that sent you.", &["parent"]),
            s("message", "What to say. Say what you found or asked, with the file:line it came from."),
        ],
        required: &["to", "message"],
        has_header: true,
        handler: Handler::SendMessage,
        // Talking is not touching: it changes no file and runs no command. What
        // the recipient does with it is judged on its own call.
        category: Category::Read,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Always,
    },
    ToolDef {
        name: "review_agent",
        description: "Record what you made of a subagent's report, once you have checked it yourself. Required before you act on a report: read the files it names, or run what it suggests, then pass the verdict — verified, refuted, or partial — and say in one line what you checked. This is how a wrong reading is caught rather than passed on.",
        params: &[
            s("agent", "The agent_id of the subagent whose report you are reviewing."),
            one_of(
                "verdict",
                "verified = you checked it and it holds; refuted = you checked and it is wrong; partial = part of it holds and you say which part.",
                &["verified", "refuted", "partial"],
            ),
            s("note", "What you checked, in one line: the file you read, or the command you ran."),
        ],
        required: &["agent", "verdict", "note"],
        has_header: true,
        handler: Handler::ReviewAgent,
        category: Category::Read,
        touches: Touches::Nothing,
        preview: Preview::None,
        availability: Availability::Parent,
    },
];

pub fn find(name: &str) -> Option<&'static ToolDef> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Whether this tool is offered in a conversation with this window and these
/// settings. `core` is false for a window too small for the longer schemas: a
/// compact window gets the ten tools a short task needs and nothing else, so
/// the web and vision tools wait with the rest of the extended set.
pub fn offered(def: &ToolDef, core: bool, vision: bool, web: bool, parent: bool) -> bool {
    match def.availability {
        Availability::Always => true,
        Availability::Core => core,
        Availability::Vision => core && vision,
        Availability::Web => core && web,
        Availability::Parent => parent,
    }
}

impl ToolDef {
    /// The JSON schema the model is sent, built from the parameters rather than
    /// pasted, so a property cannot be described in one place and typed in
    /// another.
    pub fn schema_json(&self) -> String {
        let mut props = String::from("{");
        if self.has_header {
            write_param(&mut props, &HEADER);
        }
        for param in self.params {
            write_param(&mut props, param);
        }
        props.push('}');

        let mut required: Vec<&str> = self.required.to_vec();
        if self.has_header {
            required.insert(0, "header");
        }
        let required = if required.is_empty() {
            String::new()
        } else {
            let names: Vec<String> = required.iter().map(|r| format!("\"{r}\"")).collect();
            format!(",\"required\":[{}]", names.join(","))
        };
        format!("{{\"type\":\"object\",\"properties\":{props}{required}}}")
    }
}

fn write_param(out: &mut String, param: &Param) {
    if !out.is_empty() && !out.ends_with('{') {
        out.push(',');
    }
    out.push_str(&format!("\"{}\":", param.name));
    out.push_str(&param_body(param));
}

fn param_body(param: &Param) -> String {
    let mut body = String::from("{\"type\":");
    body.push_str(match param.kind {
        Kind::Str => "\"string\"",
        Kind::Int => "\"integer\"",
        Kind::Bool => "\"boolean\"",
        Kind::Strings | Kind::Objects(_) => "\"array\"",
    });
    match param.kind {
        Kind::Strings => body.push_str(",\"items\":{\"type\":\"string\"}"),
        Kind::Objects(props) => {
            body.push_str(",\"items\":");
            body.push_str(&object_body(props));
        }
        Kind::Str | Kind::Int | Kind::Bool => {}
    }
    if !param.choices.is_empty() {
        let names: Vec<String> = param.choices.iter().map(|c| format!("\"{c}\"")).collect();
        body.push_str(&format!(",\"enum\":[{}]", names.join(",")));
    }
    if !param.description.is_empty() {
        body.push_str(&format!(",\"description\":{}", json_string(param.description)));
    }
    body.push('}');
    body
}

/// One entry of a list: an object with the properties it was declared with, and
/// only the ones that were not marked optional as required.
fn object_body(props: &[Param]) -> String {
    let mut inner = String::new();
    for prop in props {
        write_param(&mut inner, prop);
    }
    let names: Vec<String> =
        props.iter().filter(|p| !p.optional).map(|p| format!("\"{}\"", p.name)).collect();
    let required =
        if names.is_empty() { String::new() } else { format!(",\"required\":[{}]", names.join(",")) };
    format!("{{\"type\":\"object\",\"properties\":{{{inner}}}{required}}}")
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_tool_has_one_name_and_one_handler() {
        let mut names = BTreeSet::new();
        for def in TOOLS {
            assert!(names.insert(def.name), "two tools are called {}: the second is never reached", def.name);
            assert_eq!(
                def.handler.tool_name(),
                def.name,
                "{} is declared with the handler for {}: a rename that leaves the old code behind",
                def.name,
                def.handler.tool_name()
            );
        }
        // And no handler without a tool: every variant is spoken for.
        let handled: BTreeSet<&str> = TOOLS.iter().map(|t| t.handler.tool_name()).collect();
        assert_eq!(handled.len(), TOOLS.len(), "two tools share a handler");
    }

    #[test]
    fn a_tool_that_says_it_takes_a_file_says_so_where_the_paths_are_named() {
        // The permission layer asks a tool which arguments are paths. Getting
        // this wrong is not a crash: it is a read or a write the user never
        // approved, outside their project.
        for def in TOOLS {
            if def.touches != Touches::Files {
                continue;
            }
            let names: BTreeSet<&str> = def.params.iter().map(|p| p.name).collect();
            assert!(names.contains("path"), "{} touches files but names no path argument", def.name);
        }
    }

    #[test]
    fn a_tool_that_shows_a_diff_is_a_tool_that_writes() {
        for def in TOOLS {
            match def.preview {
                Preview::None => {}
                _ => assert_eq!(
                    def.category,
                    Category::Write,
                    "{} shows a diff on an approval card but is not a write",
                    def.name
                ),
            }
        }
    }

    #[test]
    fn every_tool_tells_the_model_what_it_is_for_and_every_argument_what_it_is() {
        for def in TOOLS {
            assert!(!def.description.trim().is_empty(), "{} has no description", def.name);
            for param in all_params(def) {
                if param.name.is_empty() {
                    continue;
                }
                assert!(
                    !param.description.trim().is_empty(),
                    "{}::{} is in the schema with nothing said about it",
                    def.name,
                    param.name
                );
            }
        }
    }

    #[test]
    fn the_schema_is_valid_json_and_says_what_is_required() {
        for def in TOOLS {
            let schema = def.schema_json();
            let parsed: serde_json::Value = serde_json::from_str(&schema)
                .unwrap_or_else(|e| panic!("{} has a schema that is not JSON: {e}\n{schema}", def.name));
            assert_eq!(parsed["type"], "object", "{}", def.name);
            let props = parsed["properties"].as_object().expect("no properties");
            let required: Vec<&str> =
                parsed.get("required").and_then(|r| r.as_array()).map(|a| a.iter().filter_map(|v| v.as_str()).collect()).unwrap_or_default();
            for name in &required {
                assert!(props.contains_key(*name), "{} requires {name} but does not accept it", def.name);
            }
            // A tool that narrates its call cannot do it without the note.
            if def.has_header {
                assert!(required.contains(&"header"), "{} has a header it does not require", def.name);
            }
            for (name, _) in props {
                assert!(def.params.iter().any(|p| p.name == *name) || (def.has_header && name == "header"), "{}", def.name);
            }
        }
    }

    /// The description every tool's schema used to carry, twenty-three times.
    #[test]    fn the_narrating_note_is_written_once_and_reaches_every_tool_that_takes_it() {
        let narrated: Vec<&ToolDef> = TOOLS.iter().filter(|t| t.has_header).collect();
        assert!(narrated.len() > 15, "most tools are narrated: {}", narrated.len());
        for def in narrated {
            let schema: serde_json::Value = serde_json::from_str(&def.schema_json()).unwrap();
            assert_eq!(
                schema["properties"]["header"]["description"].as_str(),
                Some(HEADER.description),
                "{} describes its header differently",
                def.name
            );
        }
        // And the two that are not narrated are not given one.
        for def in TOOLS.iter().filter(|t| !t.has_header) {
            let schema: serde_json::Value = serde_json::from_str(&def.schema_json()).unwrap();
            assert!(schema["properties"].get("header").is_none(), "{} was given a header it does not use", def.name);
        }
    }

    /// The tool list is part of the prompt, read cold on every new session, so
    /// its size is a cost and not a detail. The schemas were once hand-written
    /// JSON with a 270-character note copied into each one; a guard is cheaper
    /// than rediscovering that in a cold start. Twenty-four tools measure about
    /// 13.1k characters, so the ceiling leaves room for a few more without
    /// letting a description grow without anyone noticing.
    #[test]
    fn the_whole_tool_list_stays_a_size_a_prompt_can_carry() {
        let total: usize = TOOLS.iter().map(|d| d.schema_json().len()).sum();
        assert!(
            total < 15_000,
            "the schemas of {} tools come to {total} characters, which is read cold every session",
            TOOLS.len()
        );
        // And nothing grows without saying: a tool with a very long description
        // is usually one that is saying two things.
        for def in TOOLS {
            assert!(def.description.len() < 600, "{} has a {} character description", def.name, def.description.len());
        }
    }

    /// A model must not be made to send a field it did not choose: inside a
    /// list of objects, only the properties a tool needs are required.
    #[test]
    fn a_list_entry_requires_only_what_it_needs() {
        let schema: serde_json::Value = serde_json::from_str(&find("edit_file").unwrap().schema_json()).unwrap();
        let item = &schema["properties"]["edits"]["items"];
        assert_eq!(item["required"], serde_json::json!(["old_string", "new_string"]));
        assert!(item["properties"]["replace_all"].is_object(), "the field is still offered");
        // A plain list of strings stays a list of strings, not a list of
        // nameless objects.
        let ask: serde_json::Value = serde_json::from_str(&find("ask_user").unwrap().schema_json()).unwrap();
        assert_eq!(ask["properties"]["options"]["items"]["type"], "string");
        let questions = &ask["properties"]["questions"]["items"];
        assert_eq!(questions["type"], "object");
        assert_eq!(questions["required"], serde_json::json!(["question", "options"]));
    }

    #[test]
    fn a_tool_is_found_by_name_and_an_unknown_one_is_not() {        assert_eq!(find("write_file").unwrap().handler, Handler::WriteFile);
        assert!(find("write_files").is_none());
    }

    #[test]
    fn the_read_only_roles_can_only_be_given_tools_that_do_not_write() {
        // The role lists name tools by hand; a typo there would quietly give a
        // researcher a way to write, or nothing at all.
        for tool in crate::subagents::READ_ONLY_TOOLS {
            let def = find(tool).unwrap_or_else(|| panic!("the read-only roles name {tool}, which no tool is"));
            assert!(
                matches!(def.category, Category::Read | Category::Net),
                "a read-only role may use {tool}, which is a {:?}",
                def.category
            );
        }
    }

    fn all_params(def: &ToolDef) -> Vec<&'static Param> {
        let mut out = Vec::new();
        collect(def.params, &mut out);
        out
    }

    fn collect(params: &'static [Param], out: &mut Vec<&'static Param>) {
        for param in params {
            out.push(param);
            match param.kind {
                Kind::Objects(props) => collect(props, out),
                Kind::Str | Kind::Int | Kind::Bool | Kind::Strings => {}
            }
        }
    }
}
