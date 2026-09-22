//! Every built-in tool called the way the model calls it: name, arguments,
//! dispatcher and answer. Bugs lived at this layer unnoticed (`web_search`
//! returned nothing for months, `~/notes.txt` was looked up inside the
//! project). [`every_offered_tool_is_checked_here`] keeps this list in sync
//! with what the app offers. Network tools are in `web_live.rs`, `#[ignore]`d.

use std::collections::BTreeSet;
use std::path::PathBuf;

use flashagent_core::loop_::{ToolExec, ToolOutput};
use flashagent_llm::ToolCall;
use flashagent_tools::{BuiltinTools, BuiltinToolsConfig};

/// A few files, a subdirectory, and a git repository.
struct Project {
    dir: PathBuf,
    tools: BuiltinTools,
}

impl Project {
    fn new() -> Self {
        // A counter, not the clock: on Windows two tests could read the same time
        // and share a folder.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("flashagent-tools-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(
            dir.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n\nfn helper(x: u32) -> u32 {\n    x + 1\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "first\nsecond\nthird\n").unwrap();
        std::fs::write(dir.join("README.md"), "# Project\n\nA test project.\n").unwrap();

        let tools = BuiltinTools::new(BuiltinToolsConfig {
            cwd: dir.clone(),
            brave_api_key: None,
            question_gate: None,
            is_goal_mode: None,
            toolset_profile: Some(flashagent_core::ToolsetProfile::Full),
            web_enabled: Some(true),
            context_window: Some(200_000),
            mcp_manager: None,
        })
        .unwrap();
        // `view_image` needs vision on. `/goal` stays off: it removes the memory tools.
        tools.set_vision_supported(true);
        Project { dir, tools }
    }

    /// Requires success.
    async fn call(&self, name: &str, args: serde_json::Value) -> String {
        let out = self.raw(name, args.clone()).await;
        assert!(!out.is_error, "{name} failed on {args}: {}", out.content);
        out.content
    }

    /// Error or not.
    async fn raw(&self, name: &str, args: serde_json::Value) -> ToolOutput {
        self.tools
            .execute(&ToolCall {
                id: "t".into(),
                name: name.into(),
                args_json: args.to_string(),
            })
            .await
    }

    /// Built a component at a time, independent of the separator.
    fn read(&self, path: &str) -> String {
        let mut full = self.dir.clone();
        for part in path.split('/') {
            full.push(part);
        }
        std::fs::read_to_string(&full)
            .unwrap_or_else(|e| panic!("reading back {}: {e}", full.display()))
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const CHECKED: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "patch_file",
    "list_dir",
    "glob",
    "grep",
    "outline_file",
    "git_status",
    "git_diff",
    "run_shell",
    "env_info",
    "view_image",
    "memory_read",
    "memory_create",
    "memory_update",
    "memory_remove",
    "web_fetch",
    "web_search",
    // Need the app: `ask_user` needs someone to ask, `update_plan` a /goal run.
    // Covered by the terminal scenarios in crates/tui/tests.
    "ask_user",
    "update_plan",
];

#[tokio::test]
async fn every_offered_tool_is_checked_here() {
    let project = Project::new();
    // `update_plan` is offered only during /goal.
    project.tools.set_goal_mode(true);
    let offered: BTreeSet<String> = project.tools.specs().into_iter().map(|s| s.name).collect();
    let checked: BTreeSet<String> = CHECKED.iter().map(|s| s.to_string()).collect();

    let unchecked: Vec<_> = offered.difference(&checked).collect();
    assert!(
        unchecked.is_empty(),
        "these tools are offered to the model but nothing here runs them: {unchecked:?}"
    );
    let gone: Vec<_> = checked.difference(&offered).collect();
    assert!(gone.is_empty(), "these tools are checked here but no longer offered: {gone:?}");
}

#[tokio::test]
async fn a_file_is_read_written_and_edited() {
    let project = Project::new();

    let read = project.call("read_file", serde_json::json!({ "path": "notes.txt" })).await;
    assert!(read.contains("     1\tfirst"), "{read}");
    assert!(read.contains("     3\tthird"), "{read}");

    let window = project
        .call("read_file", serde_json::json!({ "path": "notes.txt", "offset": 1, "limit": 1 }))
        .await;
    assert_eq!(window.trim_end(), "     2\tsecond");

    project
        .call("write_file", serde_json::json!({ "path": "new/deep.txt", "content": "written\n" }))
        .await;
    // Through the tool, as the model sees it, and from disk.
    let back = project.call("read_file", serde_json::json!({ "path": "new/deep.txt" })).await;
    assert!(back.contains("written"), "written and read back through the tools: {back}");
    assert_eq!(project.read("new/deep.txt"), "written\n", "write_file must create the folders too");

    project
        .call(
            "edit_file",
            serde_json::json!({
                "path": "notes.txt",
                "edits": [{ "old_string": "second", "new_string": "SECOND" }]
            }),
        )
        .await;
    assert_eq!(project.read("notes.txt"), "first\nSECOND\nthird\n");

    // An edit that does not fit must not touch the file.
    let missed = project
        .raw(
            "edit_file",
            serde_json::json!({
                "path": "notes.txt",
                "edits": [{ "old_string": "nowhere to be found", "new_string": "x" }]
            }),
        )
        .await;
    assert!(missed.is_error, "an edit that matches nothing must fail: {}", missed.content);
    assert_eq!(project.read("notes.txt"), "first\nSECOND\nthird\n");
}

#[tokio::test]
async fn several_files_are_read_and_edited_in_one_call() {
    let project = Project::new();

    let both = project
        .call(
            "read_file",
            serde_json::json!({ "files": [{ "path": "notes.txt" }, { "path": "README.md" }] }),
        )
        .await;
    assert!(both.contains("notes.txt") && both.contains("README.md"), "{both}");
    assert!(both.contains("first") && both.contains("A test project"), "{both}");

    project
        .call(
            "edit_file",
            serde_json::json!({
                "files": [
                    { "path": "notes.txt", "edits": [{ "old_string": "first", "new_string": "1st" }] },
                    { "path": "README.md", "edits": [{ "old_string": "A test", "new_string": "One test" }] }
                ]
            }),
        )
        .await;
    assert!(project.read("notes.txt").starts_with("1st"));
    assert!(project.read("README.md").contains("One test project"));
}

#[tokio::test]
async fn a_patch_is_applied_as_a_diff() {
    let project = Project::new();
    let patch = "@@ -1,3 +1,3 @@\n first\n-second\n+patched\n third\n";
    project.call("patch_file", serde_json::json!({ "path": "notes.txt", "patch": patch })).await;
    assert_eq!(project.read("notes.txt"), "first\npatched\nthird\n");
}

#[tokio::test]
async fn the_project_is_listed_searched_and_outlined() {
    let project = Project::new();

    let listing = project.call("list_dir", serde_json::json!({ "path": "." })).await;
    assert!(listing.contains("notes.txt") && listing.contains("src/"), "{listing}");

    let globbed = project.call("glob", serde_json::json!({ "pattern": "**/*.rs" })).await;
    assert!(globbed.contains("main.rs"), "{globbed}");

    let grepped = project.call("grep", serde_json::json!({ "pattern": "fn (main|helper)" })).await;
    assert!(grepped.contains("main.rs") && grepped.contains("fn main"), "{grepped}");

    let outline = project.call("outline_file", serde_json::json!({ "path": "src/main.rs" })).await;
    assert!(outline.contains("main") && outline.contains("helper"), "{outline}");

    // No match is an answer, not a failure.
    let nothing = project.raw("grep", serde_json::json!({ "pattern": "zzzz-not-here" })).await;
    assert!(!nothing.is_error, "{}", nothing.content);
}

#[tokio::test]
async fn a_command_runs_and_a_failing_one_reports_its_exit_code() {
    let project = Project::new();

    let out = project.call("run_shell", serde_json::json!({ "command": "echo marker" })).await;
    assert!(out.contains("marker"), "{out}");
    assert!(out.contains("exit code: 0"), "{out}");

    // `exit 3` is spelled the same in both shells.
    let failed = project.raw("run_shell", serde_json::json!({ "command": "exit 3" })).await;
    assert!(failed.is_error, "a non-zero exit must be an error: {}", failed.content);
    assert!(failed.content.contains("exit code: 3"), "{}", failed.content);

    let many_lines = if cfg!(windows) {
        "for /L %i in (1,1,20000) do @echo line %i"
    } else {
        "for i in $(seq 1 20000); do echo line $i; done"
    };
    let long = project.call("run_shell", serde_json::json!({ "command": many_lines })).await;
    assert!(long.len() < 100_000, "a long output must be capped, got {} chars", long.len());
}

#[tokio::test]
async fn the_environment_and_the_repository_are_reported() {
    let project = Project::new();

    let env = project.call("env_info", serde_json::json!({})).await;
    assert!(!env.trim().is_empty(), "env_info said nothing");

    // Outside a repository both git tools must answer, not panic or hang.
    let status = project.raw("git_status", serde_json::json!({})).await;
    let diff = project.raw("git_diff", serde_json::json!({})).await;
    for out in [&status, &diff] {
        assert!(!out.content.trim().is_empty(), "a git tool said nothing outside a repository");
    }

    let git = |args: &str| {
        std::process::Command::new("git")
            .args(args.split(' '))
            .current_dir(&project.dir)
            .output()
    };
    if git("init").map(|o| o.status.success()).unwrap_or(false) {
        let _ = git("config user.email t@example.com");
        let _ = git("config user.name Test");
        let status = project.call("git_status", serde_json::json!({})).await;
        assert!(status.contains("notes.txt"), "{status}");
    }
}

#[tokio::test]
async fn a_picture_is_opened_and_a_text_file_is_not_mistaken_for_one() {
    let project = Project::new();
    // The smallest PNG header that reads as a picture.
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    png.extend_from_slice(&[0, 0, 0, 13]);
    png.extend_from_slice(b"IHDR");
    png.extend_from_slice(&8u32.to_be_bytes());
    png.extend_from_slice(&8u32.to_be_bytes());
    png.extend_from_slice(&[8, 6, 0, 0, 0]);
    png.extend_from_slice(&[0; 16]);
    std::fs::write(project.dir.join("shot.png"), &png).unwrap();

    let out = project.raw("view_image", serde_json::json!({ "path": "shot.png" })).await;
    assert!(!out.is_error, "{}", out.content);

    // Reading a picture as text must say what it is, not talk about UTF-8.
    let as_text = project.raw("read_file", serde_json::json!({ "path": "shot.png" })).await;
    assert!(as_text.is_error);
    assert!(as_text.content.contains("not a text file"), "{}", as_text.content);
}

#[tokio::test]
async fn a_memory_is_written_read_back_and_removed() {
    let project = Project::new();

    let created = project
        .raw(
            "memory_create",
            serde_json::json!({
                "scope": "project",
                "title": "Build command",
                "content": "Run cargo test --workspace before pushing."
            }),
        )
        .await;
    assert!(!created.is_error, "{}", created.content);

    let read = project.call("memory_read", serde_json::json!({ "scope": "project" })).await;
    assert!(read.contains("cargo test --workspace"), "what was written must read back: {read}");

    let updated = project
        .raw(
            "memory_update",
            serde_json::json!({
                "scope": "project",
                "title": "Build command",
                "content": "Run cargo clippy too."
            }),
        )
        .await;
    assert!(!updated.is_error, "{}", updated.content);
    let read = project.call("memory_read", serde_json::json!({ "scope": "project" })).await;
    assert!(read.contains("clippy"), "{read}");

    let removed = project
        .raw("memory_remove", serde_json::json!({ "scope": "project", "title": "Build command" }))
        .await;
    assert!(!removed.is_error, "{}", removed.content);
    let read = project.call("memory_read", serde_json::json!({ "scope": "project" })).await;
    assert!(!read.contains("clippy"), "a removed memory must be gone: {read}");
}

#[tokio::test]
async fn an_autonomous_run_may_read_memory_but_not_rewrite_it() {
    // A long unattended run must not rewrite what the agent knows.
    let project = Project::new();
    project.tools.set_goal_mode(true);
    for name in ["memory_create", "memory_update", "memory_remove"] {
        let out = project
            .raw(name, serde_json::json!({ "title": "x", "content": "y", "scope": "project" }))
            .await;
        assert!(out.is_error, "{name} rewrote memory during a /goal run");
    }
    let read = project.raw("memory_read", serde_json::json!({ "scope": "project" })).await;
    assert!(!read.is_error, "reading memory must still work: {}", read.content);
}

#[tokio::test]
async fn a_path_the_model_writes_with_a_tilde_reaches_the_home_folder() {
    let project = Project::new();
    let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) else {
        return;
    };
    let marker = PathBuf::from(home).join(format!("flashagent-tilde-{}.txt", std::process::id()));
    std::fs::write(&marker, "reached\n").unwrap();

    let name = marker.file_name().unwrap().to_string_lossy().to_string();
    let out = project.raw("read_file", serde_json::json!({ "path": format!("~/{name}") })).await;
    let _ = std::fs::remove_file(&marker);
    assert!(!out.is_error, "a tilde path was not resolved: {}", out.content);
    assert!(out.content.contains("reached"), "{}", out.content);
}

#[tokio::test]
async fn a_tool_that_is_sent_nonsense_answers_with_an_error_not_a_panic() {
    let project = Project::new();
    let nonsense = [
        ("read_file", serde_json::json!({})),
        ("read_file", serde_json::json!({ "path": "" })),
        ("write_file", serde_json::json!({ "path": "x.txt" })),
        ("edit_file", serde_json::json!({ "path": "notes.txt", "edits": [] })),
        ("patch_file", serde_json::json!({ "path": "notes.txt", "patch": "not a diff" })),
        ("glob", serde_json::json!({ "pattern": "[" })),
        ("grep", serde_json::json!({ "pattern": "(unclosed" })),
        ("list_dir", serde_json::json!({ "path": "no/such/folder" })),
        ("outline_file", serde_json::json!({ "path": "no/such/file.rs" })),
        ("run_shell", serde_json::json!({})),
        ("no_such_tool", serde_json::json!({})),
    ];
    for (name, args) in nonsense {
        let out = project.raw(name, args.clone()).await;
        assert!(out.is_error, "{name} accepted {args} instead of refusing it: {}", out.content);
        assert!(!out.content.trim().is_empty(), "{name} failed without saying why");
    }
}

#[tokio::test]
async fn a_tool_turned_off_in_settings_says_so_rather_than_running() {
    let project = Project::new();
    let tools = BuiltinTools::new(BuiltinToolsConfig {
        cwd: project.dir.clone(),
        brave_api_key: None,
        question_gate: None,
        is_goal_mode: None,
        toolset_profile: Some(flashagent_core::ToolsetProfile::Full),
        web_enabled: Some(false),
        context_window: Some(200_000),
        mcp_manager: None,
    })
    .unwrap();

    for name in ["web_fetch", "web_search"] {
        let out = tools
            .execute(&ToolCall {
                id: "t".into(),
                name: name.into(),
                args_json: serde_json::json!({ "url": "https://example.com", "query": "x" }).to_string(),
            })
            .await;
        assert!(out.is_error, "{name} ran while switched off");
        assert!(out.content.contains("Settings"), "{name}: {}", out.content);
    }
}
