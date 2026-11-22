//! Every built-in tool called the way the model calls it: name, arguments,
//! dispatcher and answer. Bugs lived at this layer unnoticed (`web_search`
//! returned nothing for months, `~/notes.txt` was looked up inside the
//! project). [`every_offered_tool_is_checked_here`] keeps this list in sync
//! with what the app offers. Network tools are in `web_live.rs`, `#[ignore]`d.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use flashagent_core::loop_::{ToolExec, ToolOutput};
use flashagent_llm::ToolCall;
use flashagent_tools::{BuiltinTools, BuiltinToolsConfig};

/// A few files, a subdirectory, and a git repository.
struct Project {
    dir: PathBuf,
    /// Shared: the subagent factory hands the same tools to every child.
    tools: Arc<BuiltinTools>,
}

impl Project {
    fn new() -> Self {
        // The default room carries a dead bridge and a dead repo config: the
        // telegram and github tools must be OFFERED here so the sync guards
        // below see the full set the model is offered in a tgbot serve.
        // Tests that really call them wire mocks through `with_bridge` /
        // `with_github`.
        Self::build(
            Some(flashagent_tools::TgBridgeConfig {
                base_url: "http://127.0.0.1:1".into(),
                token: "test".into(),
                cross_room: true,
                room: Some("dev".into()),
            }),
            Some(flashagent_tools::GitHubConfig {
                token: "test".into(),
                repo: "own/repo".into(),
                base_url: "http://127.0.0.1:1".into(),
                allowed_actions: None,
                cwd: std::env::temp_dir(),
                attachments_repo: None,
            }),
        )
    }

    /// The same room with a working mock bridge: what a tgbot serve builds.
    fn with_bridge(cfg: flashagent_tools::TgBridgeConfig) -> Self {
        Self::build(Some(cfg), None)
    }

    /// The same room with a working mock GitHub.
    fn with_github(cfg: flashagent_tools::GitHubConfig) -> Self {
        Self::build(None, Some(cfg))
    }

    fn build(
        tg: Option<flashagent_tools::TgBridgeConfig>,
        gh: Option<flashagent_tools::GitHubConfig>,
    ) -> Self {
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
        // Memory resolves through this home, never the owner's.
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        // The github tool resolves image paths against the project folder,
        // exactly as the serve does.
        let gh = gh.map(|mut cfg| {
            cfg.cwd = dir.clone();
            cfg
        });

        let tools = Arc::new(
            BuiltinTools::new(BuiltinToolsConfig {
                cwd: dir.clone(),
                web_enabled: Some(true),
                context_window: Some(200_000),
                home: Some(home),
                chat_enabled: true,
                tg_bridge: tg,
                github: gh,
                ..Default::default()
            })
            .unwrap(),
        );
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

/// The tools this file runs for real, and where the rest are checked.
///
/// The list used to be written by hand beside the code, and every tool added
/// to the registry had to be added here too, or a tool the model is offered
/// could sit with nobody running it. The registry is that list now: what is
/// declared there is what is checked, and the two cannot drift apart because
/// this list is built from the registry rather than beside it.
fn checked_here() -> BTreeSet<String> {
    flashagent_core::registry::TOOLS
        .iter()
        // The three old agent verbs are internal now; the model gets `agent`.
        .filter(|def| {
            !matches!(
                def.handler,
                flashagent_core::registry::Handler::SpawnAgent
                    | flashagent_core::registry::Handler::SendMessage
                    | flashagent_core::registry::Handler::ReviewAgent
            )
        })
        .map(|def| def.name.to_string())
        .collect()
}

/// What a test in this file really puts through `execute`. The list above
/// mirrors the registry, so comparing the two says nothing about whether
/// anything here calls a given tool: it would pass with every call deleted from
/// this file. This one is the hand-written answer, and the guard at the bottom
/// asks whether it is true.
fn really_invoked() -> BTreeSet<String> {
    [
        "read",
        "search",
        "edit",
        "run_shell",
        "env_info",
        "git",
        "web",
        "http_request",
        "skill",
        "task",
        "memory",
        "user_interaction",
        "plan",
        "chat_reply",
        "telegram",
        "tg_journal",
        "github",
        "agent",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// A model that says one thing and stops, so a child finishes at once.
struct OneWord;

#[async_trait]
impl flashagent_core::LlmSource for OneWord {
    async fn turn(
        &self,
        _messages: &[flashagent_llm::ChatMessage],
        _tools: &[flashagent_llm::ToolSpec],
    ) -> Result<
        futures::stream::BoxStream<'static, Result<flashagent_llm::LlmEvent, flashagent_llm::LlmError>>,
        flashagent_llm::LlmError,
    > {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(flashagent_llm::LlmEvent::TextDelta("child answer".into())),
            Ok(flashagent_llm::LlmEvent::Done(flashagent_llm::FinishReason::Stop)),
        ])))
    }
}

/// The set the model is really offered: the built-in tools plus `spawn_agent`,
/// which only the composite in `subagents.rs` puts in front of the model.
/// Checking `BuiltinTools` alone let `spawn_agent` fall through this guard for
/// as long as it existed.
fn what_the_model_is_offered(
    project: &Project,
) -> (
    flashagent_tools::CompositeTools,
    tokio::sync::mpsc::UnboundedReceiver<flashagent_core::SubagentOutbound>,
) {
    use flashagent_core::{PermissionMode, PermissionState};

    struct Allow;
    #[async_trait]
    impl flashagent_core::ApprovalGate for Allow {
        async fn approve(&self, _req: &flashagent_core::ApprovalRequest) -> flashagent_core::Decision {
            flashagent_core::Decision::Allow
        }
    }

    let state = Arc::new(PermissionState::new(PermissionMode::Bypass, Arc::new(Allow)));
    let (composite, finished, _control) = flashagent_tools::agent_tools(project.tools.clone(), Arc::new(OneWord), state, None);
    (composite, finished)
}

#[tokio::test]
async fn every_offered_tool_is_checked_here() {
    let project = Project::new();
    // The plan tool is offered in ordinary chat too; the list here is the model's.
    project.tools.set_goal_mode(true);
    let (composite, _finished) = what_the_model_is_offered(&project);
    let offered: BTreeSet<String> = composite.specs().into_iter().map(|s| s.name).collect();
    let checked = checked_here();

    // The list is the registry, so the only way to fail is to offer something
    // the registry does not declare: an MCP tool, or a name from a server.
    let unchecked: Vec<_> = offered.difference(&checked).collect();
    assert!(
        unchecked.is_empty(),
        "these tools are offered to the model but nothing here runs them: {unchecked:?}"
    );
    let gone: Vec<_> = checked.difference(&offered).collect();
    assert!(gone.is_empty(), "these tools are checked here but no longer offered: {gone:?}");
}

/// The guard above compares the registry with itself and would pass on an empty
/// file. This asks the question it was written to answer: is every tool the
/// model is offered actually called by a test in here?
#[tokio::test]
async fn every_tool_the_model_is_offered_is_really_invoked_by_a_test() {
    let project = Project::new();
    project.tools.set_goal_mode(true);
    let (composite, _finished) = what_the_model_is_offered(&project);
    let offered: BTreeSet<String> = composite.specs().into_iter().map(|s| s.name).collect();
    let invoked = really_invoked();
    let unrun: Vec<_> = offered.difference(&invoked).collect();
    assert!(
        unrun.is_empty(),
        "these tools reach the model but no test in this file calls them: {unrun:?}"
    );
}

#[tokio::test]
async fn a_tool_added_to_the_registry_is_covered_without_being_written_down_twice() {
    // What the hand-written list used to make people do: a tool exists in the
    // registry, so this file already claims to run it. Whether that claim is
    // true is the question, and the guard above is what answers it.
    let project = Project::new();
    project.tools.set_goal_mode(true);
    let (composite, _finished) = what_the_model_is_offered(&project);
    let offered: BTreeSet<String> = composite.specs().into_iter().map(|s| s.name).collect();
    for name in checked_here() {
        assert!(offered.contains(&name), "{name} is checked here and not offered at all");
    }
    assert!(
        flashagent_core::registry::TOOLS.len() > 15,
        "the registry is the list, not an afterthought: {}",
        flashagent_core::registry::TOOLS.len()
    );
}

#[tokio::test]
async fn a_chat_reply_confirms_the_call() {
    // The delivery is the ToolStarted event on the session's feed; the tool
    // validates the call and confirms the message it answers.
    let project = Project::new();
    let out = project
        .call(
            "chat_reply",
            serde_json::json!({"message_id": 42, "text": "держи"}),
        )
        .await;
    assert!(out.contains("msg:42"), "{out}");
}

/// A bridge that answers ok to everything: enough HTTP/1.1 for the tool's
/// reqwest client, one request per connection.
async fn mock_bridge(result: &'static str) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            // Read until the headers end, then until the body is complete.
            let mut buf = Vec::new();
            loop {
                let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
                let complete = match head_end {
                    Some(pos) => {
                        let cl = buf
                            .split(|b| *b == b'\n')
                            .find_map(|line| {
                                let line = line.trim_ascii();
                                line.to_ascii_lowercase()
                                    .strip_prefix(b"content-length:")
                                    .map(|rest| {
                                        std::str::from_utf8(rest)
                                            .unwrap_or("0")
                                            .trim()
                                            .parse::<usize>()
                                            .unwrap_or(0)
                                    })
                            })
                            .unwrap_or(0);
                        buf.len() >= pos + 4 + cl
                    }
                    None => false,
                };
                if complete { break; }
                let mut chunk = [0u8; 4096];
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 { break; }
                buf.extend_from_slice(&chunk[..n]);
            }
            let body = format!("{{\"ok\":true,\"result\":\"{result}\"}}");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_telegram_reply_travels_the_bridge() {
    // The tool guards, then posts; the bot's verdict line is the answer.
    let url = mock_bridge("ok: msg_id=77").await;
    let project = Project::with_bridge(flashagent_tools::TgBridgeConfig {
        base_url: url,
        token: "test-token".into(),
        cross_room: true,
        room: None,
    });
    let out = project
        .call(
            "telegram",
            serde_json::json!({"action": "reply", "msg_id": 77, "text": "держи"}),
        )
        .await;
    assert!(out.contains("ok: msg_id=77"), "{out}");
}

#[tokio::test]
async fn a_pin_outside_a_turn_scope_is_refused() {
    // Mutating actions are charged to the turn's budget; no scope, no call.
    let url = mock_bridge("ok").await;
    let project = Project::with_bridge(flashagent_tools::TgBridgeConfig {
        base_url: url,
        token: "t".into(),
        cross_room: false,
        room: None,
    });
    let out = project
        .raw("telegram", serde_json::json!({"action": "pin", "msg_id": 5}))
        .await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("ход не размечен"), "{}", out.content);
}

#[tokio::test]
async fn mutations_in_one_turn_are_charged_and_throttled() {
    // Реальный путь: пин проходит, второй пин застревает на троттлинге,
    // реплай (не мутация) идёт сразу. Сам потолок 8/ход — юнит-тестом в
    // telegram.rs, где счётчик можно прогреть без ожидания.
    let url = mock_bridge("ok").await;
    let project = Project::with_bridge(flashagent_tools::TgBridgeConfig {
        base_url: url,
        token: "t".into(),
        cross_room: false,
        room: None,
    });
    let outs = flashagent_tools::scope_turn(async move {
        let mut outs = Vec::new();
        outs.push(project.raw("telegram", serde_json::json!({"action": "pin", "msg_id": 1})).await);
        outs.push(project.raw("telegram", serde_json::json!({"action": "pin", "msg_id": 2})).await);
        outs.push(project.raw("telegram", serde_json::json!({"action": "reply", "msg_id": 3, "text": "держи"})).await);
        outs
    })
    .await;
    assert!(!outs[0].is_error, "the first pin goes through: {}", outs[0].content);
    assert!(outs[1].is_error, "{}", outs[1].content);
    assert!(outs[1].content.contains("подожди"), "{}", outs[1].content);
    assert!(!outs[2].is_error, "a reply is speech, not charged: {}", outs[2].content);
}


/// A mock GitHub API: the search endpoint answers with one open issue,
/// everything else answers like a created issue.
async fn mock_github() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let mut buf = Vec::new();
            loop {
                let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
                let complete = match head_end {
                    Some(pos) => {
                        let cl = buf
                            .split(|b| *b == b'\n')
                            .find_map(|line| {
                                let line = line.trim_ascii();
                                line.to_ascii_lowercase()
                                    .strip_prefix(b"content-length:")
                                    .map(|rest| {
                                        std::str::from_utf8(rest)
                                            .unwrap_or("0")
                                            .trim()
                                            .parse::<usize>()
                                            .unwrap_or(0)
                                    })
                            })
                            .unwrap_or(0);
                        buf.len() >= pos + 4 + cl
                    }
                    None => false,
                };
                if complete { break; }
                let mut chunk = [0u8; 4096];
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 { break; }
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            let body = if head.contains("GET /search/issues") {
                "{\"items\": [{\"number\": 7, \"state\": \"open\", \"title\": \"Очередь не чистится\"}]}".to_string()
            } else {
                "{\"number\": 7, \"html_url\": \"https://github.com/own/repo/issues/7\"}".to_string()
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn a_github_search_finds_the_similar_report() {
    let url = mock_github().await;
    let project = Project::with_github(flashagent_tools::GitHubConfig {
        token: "t".into(),
        repo: "own/repo".into(),
        base_url: url,
        allowed_actions: None,
        cwd: std::env::temp_dir(),
        attachments_repo: None,
        });
    let out = project
        .call(
            "github",
            serde_json::json!({"action": "search", "query": "очередь не чистится"}),
        )
        .await;
    assert!(out.contains("#7 [open]"), "{out}");
    assert!(out.contains("Очередь не чистится"), "{out}");
}

#[tokio::test]
async fn an_issue_is_filed_through_the_mock_and_names_itself() {
    let url = mock_github().await;
    let project = Project::with_github(flashagent_tools::GitHubConfig {
        token: "t".into(),
        repo: "own/repo".into(),
        base_url: url,
        allowed_actions: None,
        cwd: std::env::temp_dir(),
        attachments_repo: None,
        });
    let out = flashagent_tools::scope_turn(async {
        project
            .call(
                "github",
                serde_json::json!({"action": "create", "title": "Очередь", "body": "шаги внутри"}),
            )
            .await
    })
    .await;
    assert!(out.contains("заведён issue #7"), "{out}");
    assert!(out.contains("issues/7"), "{out}");
}



/// A mock that records every request: the attachments flow needs several
/// endpoints answered differently.
async fn mock_github_recording() -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
    use std::sync::{Arc, Mutex};
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let sink = log.clone();
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let mut buf = Vec::new();
            loop {
                let head_end = buf.windows(4).position(|w| w == b"\r\n\r\n");
                let complete = match head_end {
                    Some(pos) => {
                        let cl = buf
                            .split(|b| *b == b'\n')
                            .find_map(|line| {
                                let line = line.trim_ascii();
                                line.to_ascii_lowercase()
                                    .strip_prefix(b"content-length:")
                                    .map(|rest| {
                                        std::str::from_utf8(rest)
                                            .unwrap_or("0")
                                            .trim()
                                            .parse::<usize>()
                                            .unwrap_or(0)
                                    })
                            })
                            .unwrap_or(0);
                        buf.len() >= pos + 4 + cl
                    }
                    None => false,
                };
                if complete { break; }
                let mut chunk = [0u8; 8192];
                let n = sock.read(&mut chunk).await.unwrap_or(0);
                if n == 0 { break; }
                buf.extend_from_slice(&chunk[..n]);
            }
            let head = String::from_utf8_lossy(&buf).to_string();
            let body = String::from_utf8_lossy(
                &buf[head.find("\r\n\r\n").map(|p| p + 4).unwrap_or(buf.len())..],
            )
            .to_string();
            sink.lock().unwrap().push(format!("{} || {}", head.lines().next().unwrap_or(""), body));
            let (status, payload) = if head.starts_with("GET /user ") {
                (200, "{\"login\": \"flashagent-bot\"}".to_string())
            } else if head.starts_with("GET /repos/flashagent-bot/rx-playa-attachments ") {
                (404, "{\"message\":\"Not Found\"}".to_string())
            } else if head.starts_with("POST /user/repos ") {
                (201, "{\"full_name\": \"flashagent-bot/rx-playa-attachments\"}".to_string())
            } else if head.starts_with("PUT /repos/flashagent-bot/rx-playa-attachments/contents/") {
                (201, "{\"content\": {}}".to_string())
            } else if head.starts_with("GET /repos/own/repo ") {
                (200, "{\"default_branch\": \"main\"}".to_string())
            } else if head.contains("/git/ref/heads/flashagent-attachments") {
                (404, "{\"message\":\"Not Found\"}".to_string())
            } else if head.contains("/git/ref/heads/main") {
                (200, "{\"object\": {\"sha\": \"abc123\"}}".to_string())
            } else if head.starts_with("POST /repos/own/repo/git/refs") {
                (201, "{\"ref\": \"refs/heads/flashagent-attachments\"}".to_string())
            } else if head.starts_with("PUT /repos/own/repo/contents/") {
                (201, "{\"content\": {}}".to_string())
            } else if head.starts_with("POST /repos/own/repo/issues ") {
                (201, "{\"number\": 7, \"html_url\": \"https://github.com/own/repo/issues/7\"}".to_string())
            } else if head.starts_with("PATCH /repos/own/repo/issues/7") {
                (200, "{\"number\": 7}".to_string())
            } else {
                (200, "{}".to_string())
            };
            let resp = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
            let _ = sock.shutdown().await;
        }
    });
    (format!("http://{addr}"), log)
}

#[tokio::test]
async fn an_issue_with_images_uploads_them_and_embeds_the_markdown() {
    let (url, log) = mock_github_recording().await;
    let project = Project::with_github(flashagent_tools::GitHubConfig {
        token: "t".into(),
        repo: "own/repo".into(),
        base_url: url,
        allowed_actions: None,
        cwd: std::env::temp_dir(),
        attachments_repo: None,
        });
    std::fs::create_dir_all(project.dir.join("shots")).unwrap();
    std::fs::write(project.dir.join("shots/s1.jpg"), b"fake-jpeg-bytes").unwrap();

    let out = flashagent_tools::scope_turn(async {
        project
            .call(
                "github",
                serde_json::json!({
                    "action": "create",
                    "title": "Тест",
                    "body": "Репортёр: @u (Юзер)\n\nтело",
                    "images": ["shots/s1.jpg"],
                }),
            )
            .await
    })
    .await;
    assert!(out.contains("issue #7"), "{out}");

    let log = log.lock().unwrap().clone();
    assert!(
        log.iter().any(|l| l.starts_with("POST /user/repos ")),
        "the attachments repo was created on the bot account: {log:?}"
    );
    assert!(
        log.iter().any(|l| l.starts_with("PUT /repos/flashagent-bot/rx-playa-attachments/contents/issue-7/0-s1.jpg")),
        "the image went to the bot's attachments repo: {log:?}"
    );
    assert!(
        !log.iter().any(|l| l.starts_with("PUT /repos/own/repo/contents/")),
        "the project repo is never written to"
    );
    let patch = log.iter().find(|l| l.starts_with("PATCH /repos/own/repo/issues/7")).expect("body patched");
    assert!(patch.contains("raw.githubusercontent.com/flashagent-bot/rx-playa-attachments"), "{patch}");
}

#[tokio::test]
async fn an_image_path_that_escapes_the_project_is_refused() {
    let (url, _log) = mock_github_recording().await;
    let project = Project::with_github(flashagent_tools::GitHubConfig {
        token: "t".into(),
        repo: "own/repo".into(),
        base_url: url,
        allowed_actions: None,
        cwd: std::env::temp_dir(),
        attachments_repo: None,
        });
    let out = flashagent_tools::scope_turn(async {
        project
            .raw(
                "github",
                serde_json::json!({
                    "action": "create",
                    "title": "x",
                    "body": "y",
                    "images": ["../../etc/passwd"],
                }),
            )
            .await
    })
    .await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("относительным"), "{}", out.content);
}

#[tokio::test]
async fn a_close_without_a_reason_is_refused_before_the_network() {
    // D9: the reason lands in the issue; a silent close is a lost sentence.
    let url = mock_github().await;
    let project = Project::with_github(flashagent_tools::GitHubConfig {
        token: "t".into(),
        repo: "own/repo".into(),
        base_url: url,
        allowed_actions: None,
        cwd: std::env::temp_dir(),
        attachments_repo: None,
        });
    let out = flashagent_tools::scope_turn(async {
        project
            .raw("github", serde_json::json!({"action": "close", "number": 7}))
            .await
    })
    .await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("close требует comment"), "{}", out.content);
}

#[tokio::test]
async fn the_users_profile_cannot_close_an_issue() {
    // D5: closing is the dev room's word; the tool refuses it outright.
    let url = mock_github().await;
    let project = Project::with_github(flashagent_tools::GitHubConfig {
        token: "t".into(),
        repo: "own/repo".into(),
        base_url: url,
        allowed_actions: Some(vec!["search".into(), "create".into(), "comment".into()]),
        cwd: std::env::temp_dir(),
        attachments_repo: None,
    });
    let out = project
        .raw(
            "github",
            serde_json::json!({"action": "close", "number": 7, "comment": "готово"}),
        )
        .await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("не разрешено"), "{}", out.content);
    // The allowed ones still pass the gate (and reach the mock).
    let search = project
        .call("github", serde_json::json!({"action": "search", "query": "очередь"}))
        .await;
    assert!(search.contains("#7"), "{search}");
}

#[tokio::test]
async fn an_edit_with_nothing_to_change_is_refused() {
    let url = mock_github().await;
    let project = Project::with_github(flashagent_tools::GitHubConfig {
        token: "t".into(),
        repo: "own/repo".into(),
        base_url: url,
        allowed_actions: None,
        cwd: std::env::temp_dir(),
        attachments_repo: None,
        });
    let out = flashagent_tools::scope_turn(async {
        project
            .raw("github", serde_json::json!({"action": "edit", "number": 7}))
            .await
    })
    .await;
    assert!(out.is_error, "{}", out.content);
    assert!(out.content.contains("title или body"), "{}", out.content);
}

#[tokio::test]
async fn github_mutations_share_the_turn_budget_with_telegram() {
    // A8: one budget across both tools. The telegram pin charges the shared
    // clock; a github create fired right after is throttled BY IT — if
    // github kept its own budget, its first mutation would sail through.
    let gh_url = mock_github().await;
    let tg_url = mock_bridge("ok").await;
    let project = Project::build(
        Some(flashagent_tools::TgBridgeConfig {
            base_url: tg_url,
            token: "t".into(),
            cross_room: false,
            room: Some("users".into()),
        }),
        Some(flashagent_tools::GitHubConfig {
            token: "t".into(),
            repo: "own/repo".into(),
            base_url: gh_url,
            allowed_actions: None,
            cwd: std::env::temp_dir(),
            attachments_repo: None,
        }),
    );
    let outs = flashagent_tools::scope_turn(async move {
        let pin = project
            .raw("telegram", serde_json::json!({"action": "pin", "msg_id": 1}))
            .await;
        let create = project
            .raw(
                "github",
                serde_json::json!({"action": "create", "title": "сразу после пина", "body": "тот же бюджет"}),
            )
            .await;
        (pin, create)
    })
    .await;
    assert!(!outs.0.is_error, "{}", outs.0.content);
    assert!(outs.1.is_error, "{}", outs.1.content);
    assert!(outs.1.content.contains("подожди"), "{}", outs.1.content);
}

#[tokio::test]
async fn tg_journal_reads_through_the_bridge() {
    let url = mock_bridge("id:5 @bublik Бублик: очередь не чистится").await;
    let project = Project::with_bridge(flashagent_tools::TgBridgeConfig {
        base_url: url,
        token: "t".into(),
        cross_room: false,
        room: None,
    });
    let out = project
        .call(
            "tg_journal",
            serde_json::json!({"last": 40, "user": "@bublik", "query": "очередь"}),
        )
        .await;
    assert!(out.contains("id:5 @bublik"), "{out}");
}

#[tokio::test]
async fn a_subagent_is_started_for_real_and_answers_on_its_own() {
    // `spawn_agent` runs here the way the model calls it: through the same
    // composite, the same dispatcher and the same permission layer. It must
    // return an id rather than the child's answer, and the answer must arrive
    // on the channel the app listens to.
    let project = Project::new();
    let (composite, mut finished) = what_the_model_is_offered(&project);

    let out = composite
        .execute(&ToolCall {
            id: "t".into(),
            name: "agent".into(),
            args_json: r#"{"action":"spawn","role":"researcher","task":"read notes.txt and say what is in it"}"#.into(),
        })
        .await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("sub1"), "the call names the child it started: {}", out.content);
    assert!(!out.content.contains("child answer"), "and does not wait for it: {}", out.content);

    let answer = tokio::time::timeout(std::time::Duration::from_secs(10), finished.recv())
        .await
        .expect("a child answers without the parent asking again")
        .expect("the channel stays open");
    let flashagent_core::SubagentOutbound::Finished(answer) = answer else {
        panic!("a child that was only started says nothing else")
    };
    assert_eq!(answer.answer, "child answer");
    assert_eq!(answer.done, flashagent_core::DoneReason::Completed);
}

#[tokio::test]
async fn a_file_is_read_written_and_edited() {
    let project = Project::new();

    let read = project.call("read", serde_json::json!({ "path": "notes.txt" })).await;
    assert!(read.contains("     1\tfirst"), "{read}");
    assert!(read.contains("     3\tthird"), "{read}");

    let window = project
        .call("read", serde_json::json!({ "path": "notes.txt", "offset": 1, "limit": 1 }))
        .await;
    assert_eq!(window.trim_end(), "     2\tsecond");

    project
        .call("edit", serde_json::json!({ "mode": "write", "path": "new/deep.txt", "content": "written\n" }))
        .await;
    // Through the tool, as the model sees it, and from disk.
    let back = project.call("read", serde_json::json!({ "path": "new/deep.txt" })).await;
    assert!(back.contains("written"), "written and read back through the tools: {back}");
    assert_eq!(project.read("new/deep.txt"), "written\n", "write_file must create the folders too");

    project
        .call(
            "edit",
            serde_json::json!({
                "mode": "replace",
                "path": "notes.txt",
                "edits": [{ "old_string": "second", "new_string": "SECOND" }]
            }),
        )
        .await;
    assert_eq!(project.read("notes.txt"), "first\nSECOND\nthird\n");

    // An edit that does not fit must not touch the file.
    let missed = project
        .raw(
            "edit",
            serde_json::json!({
                "mode": "replace",
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
            "read",
            serde_json::json!({ "files": [{ "path": "notes.txt" }, { "path": "README.md" }] }),
        )
        .await;
    assert!(both.contains("notes.txt") && both.contains("README.md"), "{both}");
    assert!(both.contains("first") && both.contains("A test project"), "{both}");

    project
        .call(
            "edit",
            serde_json::json!({
                "mode": "replace",
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
    project.call("edit", serde_json::json!({ "mode": "patch", "path": "notes.txt", "patch": patch })).await;
    assert_eq!(project.read("notes.txt"), "first\npatched\nthird\n");
}

/// The model lists the edits in whatever order it thinks of them. The order
/// says nothing about where in the file the edits are, and the file must come
/// out the same either way.
#[tokio::test]
async fn edits_listed_bottom_up_are_applied_in_the_order_of_the_file() {
    let project = Project::new();
    // notes.txt holds "first\nsecond\nthird\n".
    project
        .call(
            "edit",
            serde_json::json!({
                "mode": "replace",
                "path": "notes.txt",
                "edits": [
                    { "old_string": "third", "new_string": "3" },
                    { "old_string": "first", "new_string": "1st-and-a-half" }
                ]
            }),
        )
        .await;
    assert_eq!(project.read("notes.txt"), "1st-and-a-half\nsecond\n3\n");
}

#[tokio::test]
async fn the_project_is_listed_searched_and_outlined() {
    let project = Project::new();

    let listing = project.call("search", serde_json::json!({ "mode": "list", "path": "." })).await;
    assert!(listing.contains("notes.txt") && listing.contains("src/"), "{listing}");

    let globbed = project.call("search", serde_json::json!({ "mode": "files", "pattern": "**/*.rs" })).await;
    assert!(globbed.contains("main.rs"), "{globbed}");

    let grepped = project.call("search", serde_json::json!({ "mode": "content", "pattern": "fn (main|helper)" })).await;
    assert!(grepped.contains("main.rs") && grepped.contains("fn main"), "{grepped}");

    let outline = project.call("read", serde_json::json!({ "mode": "outline", "path": "src/main.rs" })).await;
    assert!(outline.contains("main") && outline.contains("helper"), "{outline}");

    // No match is an answer, not a failure.
    let nothing = project.raw("search", serde_json::json!({ "mode": "content", "pattern": "zzzz-not-here" })).await;
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

    let many_lines = if flashagent_tools::shell::platform().contains("cmd.exe") {
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
    let status = project.raw("git", serde_json::json!({ "action": "status" })).await;
    let diff = project.raw("git", serde_json::json!({ "action": "diff" })).await;
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
        let status = project.call("git", serde_json::json!({ "action": "status" })).await;
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

    let out = project.raw("read", serde_json::json!({ "mode": "image", "path": "shot.png" })).await;
    assert!(!out.is_error, "{}", out.content);

    // Reading a picture as text must say what it is, not talk about UTF-8.
    let as_text = project.raw("read", serde_json::json!({ "mode": "text", "path": "shot.png" })).await;
    assert!(as_text.is_error);
    assert!(as_text.content.contains("not a text file"), "{}", as_text.content);
}

#[tokio::test]
async fn a_memory_is_written_read_back_and_removed() {
    let project = Project::new();

    let created = project
        .raw(
            "memory",
            serde_json::json!({
                "command": "create",
                "scope": "project",
                "path": "build-command",
                "file_text": "Run cargo test --workspace before pushing.",
                "description": "the build command"
            }),
        )
        .await;
    assert!(!created.is_error, "{}", created.content);

    let read = project
        .call("memory", serde_json::json!({ "command": "view", "scope": "project", "path": "build-command" }))
        .await;
    assert!(read.contains("cargo test --workspace"), "what was written must read back: {read}");

    let updated = project
        .raw(
            "memory",
            serde_json::json!({
                "command": "str_replace",
                "scope": "project",
                "path": "build-command",
                "old_str": "cargo test --workspace",
                "new_str": "cargo clippy too"
            }),
        )
        .await;
    assert!(!updated.is_error, "{}", updated.content);
    let read = project
        .call("memory", serde_json::json!({ "command": "view", "scope": "project", "path": "build-command" }))
        .await;
    assert!(read.contains("clippy"), "{read}");

    let removed = project
        .raw("memory", serde_json::json!({ "command": "delete", "scope": "project", "path": "build-command" }))
        .await;
    assert!(!removed.is_error, "{}", removed.content);
    let read = project.call("memory", serde_json::json!({ "command": "view", "scope": "project" })).await;
    assert!(!read.contains("clippy"), "a removed memory must be gone: {read}");
}

#[tokio::test]
async fn an_autonomous_run_may_read_memory_but_not_rewrite_it() {
    // A long unattended run must not rewrite what the agent knows.
    let project = Project::new();
    project.tools.set_goal_mode(true);
    for command in ["create", "str_replace", "delete"] {
        let out = project
            .raw(
                "memory",
                serde_json::json!({ "command": command, "path": "x", "file_text": "y", "old_str": "y", "scope": "project" }),
            )
            .await;
        assert!(out.is_error, "memory {command} rewrote memory during a /goal run");
    }
    let read = project
        .raw("memory", serde_json::json!({ "command": "view", "scope": "project" }))
        .await;
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
    let out = project.raw("read", serde_json::json!({ "path": format!("~/{name}") })).await;
    let _ = std::fs::remove_file(&marker);
    assert!(!out.is_error, "a tilde path was not resolved: {}", out.content);
    assert!(out.content.contains("reached"), "{}", out.content);
}

#[tokio::test]
async fn a_tool_that_is_sent_nonsense_answers_with_an_error_not_a_panic() {
    let project = Project::new();
    let nonsense = [
        ("read", serde_json::json!({})),
        ("read", serde_json::json!({ "path": "" })),
        ("edit", serde_json::json!({ "mode": "write", "path": "x.txt" })),
        ("edit", serde_json::json!({ "mode": "replace", "path": "notes.txt", "edits": [] })),
        ("edit", serde_json::json!({ "mode": "patch", "path": "notes.txt", "patch": "not a diff" })),
        ("search", serde_json::json!({ "mode": "files", "pattern": "[" })),
        ("search", serde_json::json!({ "mode": "content", "pattern": "(unclosed" })),
        ("search", serde_json::json!({ "mode": "list", "path": "no/such/folder" })),
        ("read", serde_json::json!({ "mode": "outline", "path": "no/such/file.rs" })),
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
        web_enabled: Some(false),
        context_window: Some(200_000),
        ..Default::default()
    })
    .unwrap();

    for (name, args) in [("web", serde_json::json!({ "action": "fetch", "url": "https://example.com" })), ("http_request", serde_json::json!({ "url": "https://example.com" }))] {
        let out = tools
            .execute(&ToolCall {
                id: "t".into(),
                name: name.into(),
                args_json: args.to_string(),
            })
            .await;
        assert!(out.is_error, "{name} ran while switched off");
        assert!(out.content.contains("Settings"), "{name}: {}", out.content);
    }
}

#[tokio::test]
async fn a_question_is_asked_through_the_composite_the_model_really_calls() {
    // `ask_user` has no side effect that could be read back, so the only honest
    // way to cover it is to call the thing the model calls and see what the
    // question gate is handed.
    let project = Project::new();
    let (composite, _out) = what_the_model_is_offered(&project);
    let out = composite
        .execute(&ToolCall {
            id: "t".into(),
            name: "user_interaction".into(),
            args_json: serde_json::json!({
                "mode": "ask",
                "question": "Which one?",
                "options": ["left", "right"],
                "multi_select": false
            })
            .to_string(),
        })
        .await;
    // With no gate behind it the tool says so rather than hanging or pretending.
    assert!(
        out.is_error || out.content.to_lowercase().contains("question") || out.content.to_lowercase().contains("wait"),
        "ask_user answered something a person never chose: {}",
        out.content
    );
}

#[tokio::test]
async fn a_plan_is_written_read_back_and_cleared_during_a_goal() {
    // `update_plan` is offered only under /goal, and only a real call proves the
    // dispatcher routes it there and not to some fallback.
    let project = Project::new();
    project.tools.set_goal_mode(true);
    let (composite, _out) = what_the_model_is_offered(&project);
    let write = composite
        .execute(&ToolCall {
            id: "t".into(),
            name: "plan".into(),
            args_json: serde_json::json!({
                "action": "create",
                "items": [
                    {"status": "in_progress", "text": "read the code"},
                    {"status": "pending", "text": "fix it"}
                ]
            })
            .to_string(),
        })
        .await;
    assert!(!write.is_error, "plan refused a list the model wrote: {}", write.content);
    // A step is marked without sending the list again.
    let write = composite
        .execute(&ToolCall {
            id: "t2".into(),
            name: "plan".into(),
            args_json: serde_json::json!({ "action": "done", "steps": ["fix it"] }).to_string(),
        })
        .await;
    assert!(!write.is_error, "plan refused a mark: {}", write.content);
}

#[tokio::test]
async fn sending_a_message_and_reviewing_a_child_run_through_the_real_composite() {
    // `send_message` and `review_agent` only exist once a child is running, so
    // this spawns one first and then uses both against its real id.
    let project = Project::new();
    let (composite, _out) = what_the_model_is_offered(&project);
    let spawn = composite
        .execute(&ToolCall {
            id: "t".into(),
            name: "agent".into(),
            args_json: serde_json::json!({ "action": "spawn", "role": "researcher", "task": "say something and finish" }).to_string(),
        })
        .await;
    assert!(!spawn.is_error, "{}", spawn.content);
    let id = spawn
        .content
        .split_whitespace()
        .find(|w| w.starts_with('#') || w.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
        .unwrap_or_default()
        .trim_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_string();
    assert!(!id.is_empty(), "no agent id to talk to: {}", spawn.content);

    let review = composite
        .execute(&ToolCall {
            id: "t".into(),
            name: "agent".into(),
            args_json: serde_json::json!({
                "action": "review",
                "agent": id,
                "verdict": "verified",
                "note": "checked the answer against the file"
            })
            .to_string(),
        })
        .await;
    // A finished child is recorded as checked, and the parent is told the
    // review happened. What matters is that the call reached the tool and came
    // back as a verdict, not that it was ignored.
    assert!(!review.is_error, "review_agent failed outright: {}", review.content);
    assert!(review.content.contains("checked"), "the review was not recorded: {}", review.content);
    assert!(review.content.contains("checked the answer against the file"), "the note was dropped: {}", review.content);

    // The child has already finished, so a message to it is refused by name,
    // listing who is actually running. That refusal is the behaviour worth
    // covering: it must not be a silent no-op.
    let to_finished = composite
        .execute(&ToolCall {
            id: "t".into(),
            name: "agent".into(),
            args_json: serde_json::json!({ "action": "message", "to": id, "message": "wrap it up" }).to_string(),
        })
        .await;
    assert!(to_finished.is_error, "a message to nobody was accepted: {}", to_finished.content);
    assert!(to_finished.content.contains("Running:"), "it must say who is running: {}", to_finished.content);

    // And the parent, which is always there to be told.
    let to_parent = composite
        .execute(&ToolCall {
            id: "t".into(),
            name: "agent".into(),
            args_json: serde_json::json!({ "action": "message", "to": "parent", "message": "the file is notes.txt:2" }).to_string(),
        })
        .await;
    assert!(!to_parent.is_error, "send_message failed outright: {}", to_parent.content);
}

#[tokio::test]
async fn a_skill_is_listed_and_read_as_instructions() {
    let project = Project::new();
    let dir = project.dir.join(".agents").join("skills");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("greet.md"), "description: Say hello\n\nSay hello to the user in one line.\n").unwrap();

    let listed = project.call("skill", serde_json::json!({ "action": "list" })).await;
    assert!(listed.contains("greet") && listed.contains("Say hello"), "{listed}");
    let read = project.call("skill", serde_json::json!({ "action": "run", "name": "greet" })).await;
    assert!(read.contains("Say hello to the user in one line."), "{read}");
    let unknown = project.raw("skill", serde_json::json!({ "action": "run", "name": "../secret" })).await;
    assert!(unknown.is_error, "a name that leaves the skills folder was accepted: {}", unknown.content);
}

#[tokio::test]
async fn a_background_task_is_listed_read_and_stopped_through_the_task_tool() {
    let project = Project::new();
    let started = project
        .call("run_shell", serde_json::json!({ "command": "echo task-output", "background": true }))
        .await;
    let id = started
        .split_whitespace()
        .find_map(|word| word.trim_matches('.').parse::<u32>().ok())
        .unwrap_or_else(|| panic!("no task id in {started:?}"));

    let listed = project.call("task", serde_json::json!({ "action": "list" })).await;
    assert!(listed.contains(&id.to_string()), "{listed}");

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let output = project.call("task", serde_json::json!({ "action": "output", "task_id": id })).await;
    assert!(output.contains("task-output") || output.contains("exited"), "{output}");

    let stopped = project.raw("task", serde_json::json!({ "action": "stop", "task_id": id })).await;
    assert!(!stopped.is_error, "{}", stopped.content);
}

#[tokio::test]
async fn an_http_request_reaches_a_named_local_address_with_headers_and_a_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (seen_tx, seen_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0u8; 8192];
        let read = socket.read(&mut request).await.unwrap_or(0);
        let _ = seen_tx.send(String::from_utf8_lossy(&request[..read]).to_string());
        let body = "request-body";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(response.as_bytes()).await;
    });

    let project = Project::new();
    let out = project
        .call(
            "http_request",
            serde_json::json!({
                "method": "POST",
                "url": format!("http://{addr}/api"),
                "headers": [{ "name": "X-FlashAgent-Test", "value": "yes" }],
                "body": "sent-body"
            }),
        )
        .await;
    assert!(out.contains("request-body"), "{out}");
    let seen = seen_rx.await.unwrap_or_default();
    assert!(seen.contains("POST /api"), "{seen}");
    assert!(seen.to_lowercase().contains("x-flashagent-test: yes"), "{seen}");
    assert!(seen.contains("sent-body"), "{seen}");
}
