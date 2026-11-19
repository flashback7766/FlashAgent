//! `ask_user`. During `/goal` nobody may be at the keyboard, so a question
//! waits [`GOAL_QUESTION_TIMEOUT`] and then tells the model to carry on with
//! its own best choice.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;
use serde::Deserialize;

use crate::ToolError;

pub const GOAL_QUESTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Implemented by the TUI.
#[async_trait]
pub trait QuestionGate: Send + Sync {
    /// `deadline` is when an unanswered question stops waiting, so the card can
    /// show it. Returns `(answer, is_write_in)`.
    async fn ask(
        &self,
        question: &str,
        options: Option<&[String]>,
        multi_select: bool,
        deadline: Option<std::time::Instant>,
    ) -> Result<(String, bool), String>;

    /// Open a URL or a path in the user's default application. A gate that
    /// cannot do it says so rather than pretending.
    async fn open(&self, _target: &str) -> Result<(), String> {
        Err("opening a file or URL is not available in this environment".into())
    }

    /// A one-way notice on the user's screen, with no answer to wait for.
    async fn notify(&self, _level: &str, _text: &str) -> Result<(), String> {
        Err("notices are not available in this environment".into())
    }
}

/// What a model wrote where a string belongs: `{"label": "A", "description": "…"}`
/// reads as "A — …", and any other object as its `key: value` pairs, not as JSON.
fn option_label(item: &serde_json::Value) -> String {
    let Some(map) = item.as_object() else { return item.to_string() };
    let text = |v: &serde_json::Value| match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    };
    let name = ["label", "title", "name", "text", "option", "value", "answer"].iter().find_map(|k| map.get(*k).map(text).filter(|t| !t.is_empty()));
    match name {
        Some(name) => match map.get("description").map(text).filter(|d| !d.is_empty()) {
            Some(desc) => format!("{name} \u{2014} {desc}"),
            None => name,
        },
        None => map.iter().map(|(k, v)| format!("{k}: {}", text(v))).collect::<Vec<_>>().join("; "),
    }
}

/// Accepts a list of strings or a comma-separated string.
fn deserialize_flexible_options<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<serde_json::Value>::deserialize(deserializer)?;
    let Some(v) = opt else { return Ok(None) };
    match v {
        serde_json::Value::Array(arr) => {
            let mut res = Vec::new();
            for item in arr {
                if let Some(s) = item.as_str() {
                    let t = s.trim();
                    if !t.is_empty() {
                        res.push(t.to_string());
                    }
                } else if !item.is_null() {
                    res.push(option_label(&item));
                }
            }
            Ok((!res.is_empty()).then_some(res))
        }
        serde_json::Value::String(s) => {
            let parts: Vec<String> = s
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            Ok((!parts.is_empty()).then_some(parts))
        }
        _ => Ok(None),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RawQuestionItem")]
pub struct QuestionItem {
    pub question: String,
    /// Up to 10.
    pub options: Option<Vec<String>>,
    pub multi_select: Option<bool>,
}

/// Claude-style items carry a short `header` beside the question; as an alias
/// of `question` it made such an item a duplicate field and failed the call.
#[derive(Deserialize)]
struct RawQuestionItem {
    #[serde(default, alias = "prompt", alias = "title", alias = "text")]
    question: Option<String>,
    #[serde(default)]
    header: Option<String>,
    #[serde(default, deserialize_with = "deserialize_flexible_options", alias = "choices", alias = "items")]
    options: Option<Vec<String>>,
    #[serde(default, alias = "is_multi_select", alias = "multiple")]
    multi_select: Option<bool>,
}

impl From<RawQuestionItem> for QuestionItem {
    fn from(raw: RawQuestionItem) -> Self {
        Self { question: raw.question.or(raw.header).unwrap_or_default(), options: raw.options, multi_select: raw.multi_select }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum QuestionItemOrString {
    Item(QuestionItem),
    Simple(String),
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AskUserArgs {
    /// Single-question mode.
    #[serde(default, alias = "prompt", alias = "title", alias = "text")]
    pub question: Option<String>,
    #[serde(default, deserialize_with = "deserialize_flexible_options", alias = "choices")]
    pub options: Option<Vec<String>>,
    #[serde(default, alias = "is_multi_select", alias = "multiple")]
    pub multi_select: Option<bool>,
    #[serde(default, alias = "items")]
    pub questions: Option<Vec<QuestionItemOrString>>,
}

pub async fn run_ask_user(
    gate: Option<&Arc<dyn QuestionGate>>,
    is_goal_mode: &Arc<AtomicBool>,
    args: AskUserArgs,
) -> Result<String, ToolError> {
    run_ask_user_within(gate, is_goal_mode, args, GOAL_QUESTION_TIMEOUT).await
}

/// With the `/goal` wait as a parameter, so tests need not wait two minutes.
async fn run_ask_user_within(
    gate: Option<&Arc<dyn QuestionGate>>,
    is_goal_mode: &Arc<AtomicBool>,
    args: AskUserArgs,
    goal_timeout: Duration,
) -> Result<String, ToolError> {
    // One deadline for the whole call, not one per question.
    let deadline = is_goal_mode.load(Ordering::Relaxed).then(|| tokio::time::Instant::now() + goal_timeout);
    // What is said when nobody answers is how long it really waited, not the
    // limit it was given.
    let started = std::time::Instant::now();

    let gate = gate.ok_or_else(|| {
        ToolError::Other("interactive question gate is not available in this environment".into())
    })?;

    let mut items: Vec<QuestionItem> = Vec::new();
    if let Some(qs) = args.questions {
        for q in qs {
            match q {
                QuestionItemOrString::Item(item) => {
                    if !item.question.trim().is_empty() {
                        items.push(item);
                    }
                }
                QuestionItemOrString::Simple(s) => {
                    if !s.trim().is_empty() {
                        items.push(QuestionItem {
                            question: s,
                            options: None,
                            multi_select: None,
                        });
                    }
                }
            }
        }
    }
    if items.is_empty() {
        if let Some(q) = args.question {
            if !q.trim().is_empty() {
                items.push(QuestionItem {
                    question: q,
                    options: args.options,
                    multi_select: args.multi_select,
                });
            }
        }
    }

    if items.is_empty() {
        return Err(ToolError::Other("`question` or `questions` cannot be empty".into()));
    }

    // 1..10 options; extra ones are cut.
    for item in &mut items {
        if let Some(ref mut opts) = item.options {
            if opts.is_empty() {
                item.options = None;
            } else if opts.len() > 10 {
                opts.truncate(10);
            }
        }
    }

    let mut answered: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let question = item.question.trim();
        let multi_select = item.multi_select.unwrap_or(false);
        let asked = gate.ask(question, item.options.as_deref(), multi_select, deadline.map(|d| d.into_std()));
        let reply = match deadline {
            Some(d) => match tokio::time::timeout_at(d, asked).await {
                Ok(reply) => reply,
                // Dropping the pending ask takes its card off the screen.
                Err(_) => return Ok(no_answer(&answered, started.elapsed())),
            },
            None => asked.await,
        };
        let (answer, is_write_in) = match reply {
            Ok(reply) => reply,
            // What was answered before still counts.
            Err(why) if !answered.is_empty() => return Err(ToolError::Other(format!("{why}\nAnswered before that:\n{}", answered.join("\n")))),
            Err(why) => return Err(ToolError::Other(why)),
        };

        let answer_trimmed = answer.trim();
        let final_answer = if is_write_in {
            format!("{answer_trimmed} (write-in)")
        } else {
            answer_trimmed.to_string()
        };
        answered.push(if items.len() == 1 { final_answer } else { format!("{}. {}: {}", i + 1, question, final_answer) });
    }
    Ok(answered.join("\n"))
}

fn no_answer(answered: &[String], waited: Duration) -> String {
    let waited = if waited < Duration::from_secs(1) {
        "less than a second".to_string()
    } else {
        let secs = waited.as_secs();
        if secs >= 60 && secs.is_multiple_of(60) { format!("{} minutes", secs / 60) } else { format!("{secs} seconds") }
    };
    let mut out = String::new();
    if !answered.is_empty() {
        out.push_str(&format!("Answered before the user went quiet:\n{}\n\n", answered.join("\n")));
    }
    out.push_str(&format!(
        "No answer within {waited}: the user is not at the keyboard. Choose the most reasonable \
         option yourself, carry on, and name that choice in your final summary."
    ));
    out
}


// ---------------------------------------------------------------------------
// user_interaction: one tool, several ways to reach the person.
// ---------------------------------------------------------------------------

/// A field of a small form. Values are plain text; secrets are never asked for
/// here, and the prompt says so.
#[derive(Debug, Clone, Deserialize)]
pub struct FormField {
    pub name: String,
    #[serde(default)]
    pub label: Option<String>,
    /// text, number, choice or path. Text when omitted.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<String>>,
    #[serde(default)]
    pub placeholder: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StepItem {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UserInteractionArgs {
    /// ask | attach | handoff | open | form | steps | notify
    pub mode: String,
    /// Why the person is being asked; shown above the card.
    #[serde(default)]
    pub why: Option<String>,
    #[serde(flatten)]
    pub ask: AskUserArgs,
    /// attach: what to attach.
    #[serde(default)]
    pub prompt: Option<String>,
    /// attach: any, image, text or file.
    #[serde(default)]
    pub accept: Option<String>,
    #[serde(default)]
    pub multiple: Option<bool>,
    /// handoff/open/form/steps may give a deadline in an ordinary turn too.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// handoff: the file the person edits.
    #[serde(default)]
    pub path: Option<String>,
    /// handoff: what they are asked to change.
    #[serde(default)]
    pub instructions: Option<String>,
    /// open: a URL or a path.
    #[serde(default)]
    pub target: Option<String>,
    /// form/steps: a title for the card.
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub fields: Vec<FormField>,
    #[serde(default)]
    pub steps: Vec<StepItem>,
    #[serde(default)]
    pub allow_back: Option<bool>,
    /// notify: info, success, warning or error.
    #[serde(default)]
    pub level: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

async fn ask_once(
    gate: &Arc<dyn QuestionGate>,
    question: &str,
    options: Option<&[String]>,
    multi_select: bool,
    deadline: Option<std::time::Instant>,
) -> Result<(String, bool), ToolError> {
    let asked = gate.ask(question, options, multi_select, deadline);
    match deadline {
        Some(d) => tokio::time::timeout_at(tokio::time::Instant::from_std(d), asked)
            .await
            .map_err(|_| ToolError::Other("no answer before the deadline".into()))?
            .map_err(ToolError::Other),
        None => asked.await.map_err(ToolError::Other),
    }
}

/// The user typed paths, one per line or comma-separated; a path outside the
/// project is refused rather than followed.
fn attached_paths(cwd: &std::path::Path, answer: &str) -> Vec<std::path::PathBuf> {
    answer
        .split(['\n', ','])
        .map(str::trim)
        .map(|p| p.trim_matches(['"', '\'', '`']).trim())
        .filter(|p| !p.is_empty())
        .map(|p| flashagent_core::resolve_path(cwd, p))
        .filter(|p| p.is_file())
        .filter(|p| flashagent_core::path_is_inside(cwd, &p.to_string_lossy()))
        .collect()
}

fn kind_of(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") => "image",
        Some("log" | "txt" | "md" | "json" | "toml" | "yaml" | "yml") => "text",
        _ => "file",
    }
}

/// One entry point for the person at the keyboard.
pub async fn run_user_interaction(
    cwd: &std::path::Path,
    gate: Option<&Arc<dyn QuestionGate>>,
    is_goal_mode: &Arc<AtomicBool>,
    args: UserInteractionArgs,
    goal_timeout: Duration,
) -> Result<String, ToolError> {
    let gate = gate.ok_or_else(|| {
        ToolError::Other("interactive question gate is not available in this environment".into())
    })?;
    let mode = args.mode.trim().to_ascii_lowercase();
    let goal = is_goal_mode.load(Ordering::Relaxed);
    let why = args.why.as_deref().map(str::trim).filter(|w| !w.is_empty());
    let title = args.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let with_why = |question: String| -> String {
        match why {
            Some(why) => format!("{why}\n\n{question}"),
            None => question,
        }
    };

    match mode.as_str() {
        "ask" => {
            let mut ask = args.ask;
            if let Some(why) = why {
                if let Some(question) = ask.question.as_mut() {
                    *question = format!("{why}\n\n{question}");
                }
                if let Some(items) = ask.questions.as_mut() {
                    for item in items {
                        match item {
                            QuestionItemOrString::Item(q) => q.question = format!("{why}\n\n{}", q.question),
                            QuestionItemOrString::Simple(q) => *q = format!("{why}\n\n{q}"),
                        }
                    }
                }
            }
            run_ask_user_within(Some(gate), is_goal_mode, ask, goal_timeout).await
        }
        "notify" => {
            let text = args
                .text
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ToolError::Other("user_interaction notify needs text".into()))?;
            let level = args.level.as_deref().unwrap_or("info").trim().to_ascii_lowercase();
            let level = match level.as_str() {
                "info" | "success" | "warning" | "error" => level,
                _ => return Err(ToolError::Other("user_interaction notify: level is info, success, warning or error".into())),
            };
            gate.notify(&level, text).await.map_err(ToolError::Other)?;
            Ok(format!("Notified the user ({level})."))
        }
        "open" => {
            let target = args
                .target
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| ToolError::Other("user_interaction open needs a target".into()))?;
            let is_url = target.starts_with("http://") || target.starts_with("https://");
            if !is_url {
                let path = flashagent_core::resolve_path(cwd, target);
                let path = path.canonicalize().map_err(|_| ToolError::Other(format!("no such file or folder: {target}")))?;
                if !flashagent_core::path_is_inside(cwd, &path.to_string_lossy()) {
                    return Err(ToolError::Other(format!(
                        "{target} is outside the project; open it yourself, or hand the file off inside the project"
                    )));
                }
                gate.open(&path.to_string_lossy()).await.map_err(ToolError::Other)?;
                return Ok(format!("Opened {}.", path.display()));
            }
            gate.open(target).await.map_err(ToolError::Other)?;
            Ok(format!("Opened {target}."))
        }
        "attach" => {
            if goal {
                return Err(ToolError::Other(
                    "user_interaction attach is not available in an autonomous /goal: nobody can attach a file there".into(),
                ));
            }
            let prompt = args
                .prompt
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .unwrap_or("Attach a file: type its path, or drop it here; one path per line.");
            let accept = args.accept.as_deref().unwrap_or("any");
            let deadline = args.timeout_secs.map(|s| std::time::Instant::now() + Duration::from_secs(s.min(3600)));
            let question = with_why(format!("{prompt}\n\n(accepts: {accept})"));
            let (answer, _) = ask_once(gate, &question, None, false, deadline).await?;
            let paths = attached_paths(cwd, &answer);
            if paths.is_empty() {
                return Ok("No files were attached.".to_string());
            }
            let listed: Vec<String> = paths
                .iter()
                .map(|p| format!("- {} ({})", p.display(), kind_of(p)))
                .collect();
            Ok(format!(
                "Attached {} file(s):\n{}\n\nRead one with read; an image opens with mode image.",
                paths.len(),
                listed.join("\n")
            ))
        }
        "handoff" => {
            if goal {
                return Err(ToolError::Other(
                    "user_interaction handoff is not available in an autonomous /goal: nobody can edit a file there".into(),
                ));
            }
            let raw = args
                .path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| ToolError::Other("user_interaction handoff needs path".into()))?;
            let path = flashagent_core::resolve_path(cwd, raw);
            let path = path.canonicalize().map_err(|_| ToolError::Other(format!("no such file: {raw}")))?;
            let before = std::fs::read_to_string(&path)
                .map_err(|e| ToolError::Other(format!("handoff reads text files only: {raw}: {e}")))?;
            gate.open(&path.to_string_lossy())
                .await
                .map_err(|e| ToolError::Other(format!("could not open {raw}: {e}")))?;
            let instructions = args.instructions.as_deref().map(str::trim).filter(|i| !i.is_empty()).unwrap_or("");
            let question = with_why(format!(
                "Edit {} in the application that just opened.{}\nThen press Done.",
                path.display(),
                if instructions.is_empty() { String::new() } else { format!("\n\n{instructions}") }
            ));
            let (answer, _) = ask_once(gate, &question, Some(&["Done".into(), "Cancel".into()]), false, None).await?;
            if !answer.eq_ignore_ascii_case("done") {
                return Ok("No handoff: the user cancelled.".to_string());
            }
            let after = std::fs::read_to_string(&path)
                .map_err(|e| ToolError::Other(format!("could not read {} after the handoff: {e}", path.display())))?;
            if after == before {
                return Ok(format!("Handoff done: {} did not change.", path.display()));
            }
            let diff = flashagent_core::unified(Some(&before), &after, &path.to_string_lossy(), 3);
            Ok(format!("Handoff changed {}:\n{diff}", path.display()))
        }
        "form" => {
            if goal {
                return Err(ToolError::Other(
                    "user_interaction form is not available in an autonomous /goal: nobody can fill it there".into(),
                ));
            }
            if args.fields.is_empty() {
                return Err(ToolError::Other("user_interaction form needs fields".into()));
            }
            let deadline = args.timeout_secs.map(|s| std::time::Instant::now() + Duration::from_secs(s.min(3600)));
            let mut values: Vec<(String, String)> = Vec::new();
            for field in &args.fields {
                let label = field.label.as_deref().filter(|l| !l.is_empty()).unwrap_or(&field.name);
                let mut question = match title {
                    Some(title) => format!("{title}\n\n{label}"),
                    None => label.to_string(),
                };
                if let Some(hint) = field.placeholder.as_deref().filter(|p| !p.is_empty()) {
                    question.push_str(&format!("\n{hint}"));
                }
                if let Some(default) = field.default.as_deref().filter(|d| !d.is_empty()) {
                    question.push_str(&format!("\n(leave empty for {default})"));
                }
                let question = with_why(question);
                let (answer, _) = ask_once(gate, &question, field.options.as_deref(), false, deadline).await?;
                let answer = answer.trim().to_string();
                let answer = if answer.is_empty() { field.default.clone().unwrap_or_default() } else { answer };
                if field.required.unwrap_or(true) && answer.is_empty() {
                    return Err(ToolError::Other(format!("form field '{}' is required", field.name)));
                }
                let kind = field.kind.as_deref().unwrap_or("text");
                if kind == "number" && !answer.is_empty() && answer.parse::<f64>().is_err() {
                    return Err(ToolError::Other(format!("form field '{}' must be a number", field.name)));
                }
                values.push((field.name.clone(), answer));
            }
            Ok(format!(
                "Form answers:\n{}",
                values.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n")
            ))
        }
        "steps" => {
            if goal {
                return Err(ToolError::Other(
                    "user_interaction steps is not available in an autonomous /goal: nobody can follow them there".into(),
                ));
            }
            if args.steps.is_empty() {
                return Err(ToolError::Other("user_interaction steps needs steps".into()));
            }
            let total = args.steps.len();
            let mut at = 0usize;
            let mut stopped = false;
            while at < total {
                let step = &args.steps[at];
                let body = step.body.as_deref().unwrap_or("").trim();
                let heading = title.map(|t| format!("{t}\n\n")).unwrap_or_default();
                let question = with_why(format!(
                    "{heading}Step {}/{}: {}{}",
                    at + 1,
                    total,
                    step.title,
                    if body.is_empty() { String::new() } else { format!("\n\n{body}") }
                ));
                let mut options: Vec<String> = vec!["Next".into()];
                if at > 0 && args.allow_back.unwrap_or(true) {
                    options.push("Back".into());
                }
                options.push("Stop".into());
                let (answer, _) = ask_once(gate, &question, Some(&options), false, None).await?;
                match answer.trim().to_ascii_lowercase().as_str() {
                    "next" => at += 1,
                    "back" if at > 0 => at -= 1,
                    "stop" | "cancel" => {
                        stopped = true;
                        break;
                    }
                    other => return Err(ToolError::Other(format!("steps: unexpected answer '{other}'"))),
                }
            }
            if stopped {
                Ok(format!("Steps stopped at {}/{}: the user stopped the walkthrough.", at, total))
            } else {
                Ok(format!("Steps completed: {total}/{total}."))
            }
        }
        other => Err(ToolError::Other(format!(
            "user_interaction: unknown mode '{other}'; use ask, attach, handoff, open, form, steps or notify"
        ))),
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    struct MockGate {
        responses: parking_lot::Mutex<Vec<(String, bool)>>,
    }

    impl MockGate {
        fn single(resp: (String, bool)) -> Self {
            Self {
                responses: parking_lot::Mutex::new(vec![resp]),
            }
        }

        fn multiple(resps: Vec<(String, bool)>) -> Self {
            Self {
                responses: parking_lot::Mutex::new(resps),
            }
        }
    }

    #[async_trait]
    impl QuestionGate for MockGate {
        async fn ask(
            &self,
            _question: &str,
            _options: Option<&[String]>,
            _multi_select: bool,
            _deadline: Option<std::time::Instant>,
        ) -> Result<(String, bool), String> {
            let mut lock = self.responses.lock();
            if lock.is_empty() {
                Ok(("default".into(), false))
            } else {
                Ok(lock.remove(0))
            }
        }
    }

    /// Answers the first `answers` questions, then never again: the user who
    /// walked away mid-run. Records the deadline it was given.
    struct WalksAwayGate {
        answers: parking_lot::Mutex<usize>,
        deadline_seen: parking_lot::Mutex<Option<std::time::Instant>>,
    }

    #[async_trait]
    impl QuestionGate for WalksAwayGate {
        async fn ask(
            &self,
            _question: &str,
            _options: Option<&[String]>,
            _multi_select: bool,
            deadline: Option<std::time::Instant>,
        ) -> Result<(String, bool), String> {
            *self.deadline_seen.lock() = deadline;
            let answer_now = {
                let mut left = self.answers.lock();
                let yes = *left > 0;
                *left = left.saturating_sub(1);
                yes
            };
            if answer_now {
                return Ok(("SQLite".into(), false));
            }
            std::future::pending().await
        }
    }

    fn database_question() -> AskUserArgs {
        AskUserArgs {
            question: Some("Choose a database".into()),
            options: Some(vec!["SQLite".into(), "Postgres".into()]),
            multi_select: None,
            questions: None,
        }
    }

    fn interaction(mode: &str) -> UserInteractionArgs {
        UserInteractionArgs { mode: mode.to_string(), ..Default::default() }
    }

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    struct NoticeGate {
        notices: parking_lot::Mutex<Vec<(String, String)>>,
        opened: parking_lot::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl QuestionGate for NoticeGate {
        async fn ask(&self, _q: &str, _o: Option<&[String]>, _m: bool, _d: Option<std::time::Instant>) -> Result<(String, bool), String> {
            Ok(("answer".into(), false))
        }
        async fn open(&self, target: &str) -> Result<(), String> {
            self.opened.lock().push(target.to_string());
            Ok(())
        }
        async fn notify(&self, level: &str, text: &str) -> Result<(), String> {
            self.notices.lock().push((level.to_string(), text.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_form_collects_several_fields_in_one_call() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::multiple(vec![
            ("8080".into(), false),
            ("SQLite".into(), false),
        ]));
        let cwd = temp_dir();
        let mut args = interaction("form");
        args.title = Some("Server".into());
        args.fields = vec![
            FormField { name: "port".into(), label: Some("Port".into()), kind: Some("number".into()), options: None, placeholder: None, required: Some(true), default: None },
            FormField { name: "database".into(), label: Some("Database".into()), kind: Some("choice".into()), options: Some(vec!["SQLite".into(), "Postgres".into()]), placeholder: None, required: Some(true), default: None },
        ];
        let out = run_user_interaction(cwd.path(), Some(&gate), &Arc::new(AtomicBool::new(false)), args, Duration::from_secs(1)).await.unwrap();
        assert!(out.contains("port: 8080"), "{out}");
        assert!(out.contains("database: SQLite"), "{out}");
    }

    #[tokio::test]
    async fn steps_walk_forward_back_and_stop() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::multiple(vec![
            ("Next".into(), false),
            ("Back".into(), false),
            ("Next".into(), false),
            ("Stop".into(), false),
        ]));
        let cwd = temp_dir();
        let mut args = interaction("steps");
        args.steps = vec![
            StepItem { title: "First".into(), body: None },
            StepItem { title: "Second".into(), body: None },
        ];
        let out = run_user_interaction(cwd.path(), Some(&gate), &Arc::new(AtomicBool::new(false)), args, Duration::from_secs(1)).await.unwrap();
        assert!(out.contains("stopped at 1/2"), "{out}");
    }

    #[tokio::test]
    async fn attach_returns_only_files_inside_the_project() {
        let cwd = temp_dir();
        let inside = cwd.path().join("shot.png");
        std::fs::write(&inside, b"png").unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::single((format!("{}\n{}", inside.display(), outside.path().display()), false)));
        let mut args = interaction("attach");
        args.prompt = Some("Attach a screenshot".into());
        let out = run_user_interaction(cwd.path(), Some(&gate), &Arc::new(AtomicBool::new(false)), args, Duration::from_secs(1)).await.unwrap();
        assert!(out.contains("shot.png"), "{out}");
        assert!(!out.contains(&outside.path().display().to_string()), "a path outside the project was attached: {out}");
    }

    struct HandoffGate {
        path: std::path::PathBuf,
        edited: String,
    }

    #[async_trait]
    impl QuestionGate for HandoffGate {
        async fn ask(&self, _q: &str, _o: Option<&[String]>, _m: bool, _d: Option<std::time::Instant>) -> Result<(String, bool), String> {
            std::fs::write(&self.path, &self.edited).unwrap();
            Ok(("Done".into(), false))
        }
        async fn open(&self, _target: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn handoff_returns_the_diff_the_user_made() {
        let cwd = temp_dir();
        let path = cwd.path().join("style.css");
        std::fs::write(&path, "body { color: red; }\n").unwrap();
        let gate: Arc<dyn QuestionGate> = Arc::new(HandoffGate {
            path: path.clone(),
            edited: "body { color: blue; }\n".into(),
        });
        let mut args = interaction("handoff");
        args.path = Some("style.css".into());
        args.instructions = Some("Make it blue".into());
        let out = run_user_interaction(cwd.path(), Some(&gate), &Arc::new(AtomicBool::new(false)), args, Duration::from_secs(1)).await.unwrap();
        assert!(out.contains("Handoff changed"), "{out}");
        assert!(out.contains("-body { color: red; }"), "{out}");
        assert!(out.contains("+body { color: blue; }"), "{out}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "body { color: blue; }\n");
    }

    #[tokio::test]
    async fn notify_and_open_reach_the_gate() {
        let concrete = Arc::new(NoticeGate { notices: parking_lot::Mutex::new(Vec::new()), opened: parking_lot::Mutex::new(Vec::new()) });
        let gate: Arc<dyn QuestionGate> = concrete.clone();
        let goal = Arc::new(AtomicBool::new(false));
        let cwd = temp_dir();

        let mut notice = interaction("notify");
        notice.level = Some("warning".into());
        notice.text = Some("Migration is running".into());
        run_user_interaction(cwd.path(), Some(&gate), &goal, notice, Duration::from_secs(1)).await.unwrap();
        assert_eq!(concrete.notices.lock().as_slice(), [("warning".to_string(), "Migration is running".to_string())]);

        let mut open = interaction("open");
        open.target = Some("https://docs.rs/serde".into());
        run_user_interaction(cwd.path(), Some(&gate), &goal, open, Duration::from_secs(1)).await.unwrap();
        assert_eq!(concrete.opened.lock().as_slice(), ["https://docs.rs/serde"]);
    }

    #[tokio::test]
    async fn attach_handoff_form_and_steps_do_not_wait_in_a_goal() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::single(("Done".into(), false)));
        let goal = Arc::new(AtomicBool::new(true));
        let cwd = temp_dir();
        for mode in ["attach", "handoff", "form", "steps"] {
            let mut args = interaction(mode);
            if mode == "handoff" {
                let path = cwd.path().join("a.txt");
                std::fs::write(&path, "x").unwrap();
                args.path = Some(path.display().to_string());
            }
            if mode == "form" {
                args.fields = vec![FormField { name: "a".into(), label: None, kind: None, options: None, placeholder: None, required: None, default: None }];
            }
            if mode == "steps" {
                args.steps = vec![StepItem { title: "a".into(), body: None }];
            }
            let err = run_user_interaction(cwd.path(), Some(&gate), &goal, args, Duration::from_secs(1)).await.unwrap_err().to_string();
            assert!(err.contains("/goal"), "{mode}: {err}");
        }
    }

    #[tokio::test]
    async fn during_a_goal_a_question_is_still_asked_and_answered() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::single(("Option 1".into(), false)));
        let goal_flag = Arc::new(AtomicBool::new(true));
        let res = run_ask_user(Some(&gate), &goal_flag, database_question()).await.unwrap();
        assert_eq!(res, "Option 1");
    }

    #[tokio::test]
    async fn during_a_goal_an_unanswered_question_lets_the_run_carry_on() {
        let concrete = Arc::new(WalksAwayGate { answers: parking_lot::Mutex::new(0), deadline_seen: parking_lot::Mutex::new(None) });
        let gate: Arc<dyn QuestionGate> = concrete.clone();
        let goal_flag = Arc::new(AtomicBool::new(true));
        let started = std::time::Instant::now();
        let res = run_ask_user_within(Some(&gate), &goal_flag, database_question(), Duration::from_millis(80))
            .await
            .expect("an unanswered question is not an error: the run goes on");
        assert!(res.contains("No answer within"), "{res}");
        assert!(res.contains("Choose the most reasonable option yourself"), "{res}");
        // How long it really waited, not the limit it was given.
        assert!(res.contains("less than a second"), "{res}");
        assert!(!res.contains("2 minutes"), "{res}");
        assert!(started.elapsed() < Duration::from_secs(5), "it must stop waiting at the deadline");
        assert!(concrete.deadline_seen.lock().is_some(), "the card is told when it stops waiting");
    }

    #[tokio::test]
    async fn answers_given_before_the_user_went_quiet_are_kept() {
        let gate: Arc<dyn QuestionGate> =
            Arc::new(WalksAwayGate { answers: parking_lot::Mutex::new(1), deadline_seen: parking_lot::Mutex::new(None) });
        let goal_flag = Arc::new(AtomicBool::new(true));
        let args = AskUserArgs {
            question: None,
            options: None,
            multi_select: None,
            questions: Some(vec![
                QuestionItemOrString::Simple("Which database?".into()),
                QuestionItemOrString::Simple("Which port?".into()),
            ]),
        };
        let res = run_ask_user_within(Some(&gate), &goal_flag, args, Duration::from_millis(80)).await.unwrap();
        assert!(res.contains("1. Which database?: SQLite"), "{res}");
        assert!(res.contains("No answer within"), "{res}");
    }

    #[tokio::test]
    async fn a_question_closed_after_another_was_answered_keeps_that_answer() {
        // Answers the first question, then the user closes the card.
        struct ClosesSecond(parking_lot::Mutex<usize>);
        #[async_trait]
        impl QuestionGate for ClosesSecond {
            async fn ask(&self, _q: &str, _o: Option<&[String]>, _m: bool, _d: Option<std::time::Instant>) -> Result<(String, bool), String> {
                let mut asked = self.0.lock();
                *asked += 1;
                if *asked == 1 { Ok(("SQLite".into(), false)) } else { Err("the user closed the question without answering.".into()) }
            }
        }
        let gate: Arc<dyn QuestionGate> = Arc::new(ClosesSecond(parking_lot::Mutex::new(0)));
        let args = AskUserArgs {
            question: None,
            options: None,
            multi_select: None,
            questions: Some(vec![QuestionItemOrString::Simple("Which database?".into()), QuestionItemOrString::Simple("Which port?".into())]),
        };
        let err = run_ask_user(Some(&gate), &Arc::new(AtomicBool::new(false)), args).await.unwrap_err().to_string();
        assert!(err.contains("closed the question without answering"), "{err}");
        assert!(err.contains("1. Which database?: SQLite"), "{err}");
    }

    #[tokio::test]
    async fn outside_a_goal_a_question_waits_for_as_long_as_it_takes() {
        let concrete = Arc::new(WalksAwayGate { answers: parking_lot::Mutex::new(0), deadline_seen: parking_lot::Mutex::new(None) });
        let gate: Arc<dyn QuestionGate> = concrete.clone();
        let goal_flag = Arc::new(AtomicBool::new(false));
        let waited = tokio::time::timeout(
            Duration::from_millis(300),
            run_ask_user_within(Some(&gate), &goal_flag, database_question(), Duration::from_millis(80)),
        )
        .await;
        assert!(waited.is_err(), "the goal wait must not apply to an ordinary chat question");
        assert!(concrete.deadline_seen.lock().is_none());
    }

    #[tokio::test]
    async fn ask_user_choice_selection() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::single(("SQLite".into(), false)));
        let goal_flag = Arc::new(AtomicBool::new(false));

        let res = run_ask_user(
            Some(&gate),
            &goal_flag,
            AskUserArgs {
                question: Some("Choose a database".into()),
                options: Some(vec!["SQLite".into(), "Postgres".into()]),
                multi_select: None,
                questions: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(res, "SQLite");
    }

    #[tokio::test]
    async fn ask_user_write_in_appends_tag() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::single(("DuckDB".into(), true)));
        let goal_flag = Arc::new(AtomicBool::new(false));

        let res = run_ask_user(
            Some(&gate),
            &goal_flag,
            AskUserArgs {
                question: Some("Choose a database".into()),
                options: Some(vec!["SQLite".into(), "Postgres".into()]),
                multi_select: None,
                questions: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(res, "DuckDB (write-in)");
    }

    #[tokio::test]
    async fn ask_user_accepts_single_and_many_options_without_error() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::single(("Answer".into(), false)));
        let goal_flag = Arc::new(AtomicBool::new(false));

        let res_single = run_ask_user(
            Some(&gate),
            &goal_flag,
            AskUserArgs {
                question: Some("Valid question".into()),
                options: Some(vec!["Single option".into()]),
                multi_select: None,
                questions: None,
            },
        )
        .await;
        assert!(res_single.is_ok());

        let res_many = run_ask_user(
            Some(&gate),
            &goal_flag,
            AskUserArgs {
                question: Some("Valid question".into()),
                options: Some(vec![
                    "1".into(), "2".into(), "3".into(), "4".into(),
                    "5".into(), "6".into(), "7".into(),
                ]),
                multi_select: Some(true),
                questions: None,
            },
        )
        .await;
        assert!(res_many.is_ok());
    }

    #[tokio::test]
    async fn ask_user_multi_questions_sequential() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::multiple(vec![
            ("Python".into(), false),
            ("arrays".into(), false),
            ("coding challenge".into(), false),
        ]));
        let goal_flag = Arc::new(AtomicBool::new(false));

        let res = run_ask_user(
            Some(&gate),
            &goal_flag,
            AskUserArgs {
                question: None,
                options: None,
                multi_select: None,
                questions: Some(vec![
                    QuestionItemOrString::Item(QuestionItem {
                        question: "Language?".into(),
                        options: Some(vec!["Python".into(), "Rust".into(), "Go".into()]),
                        multi_select: None,
                    }),
                    QuestionItemOrString::Item(QuestionItem {
                        question: "Topic?".into(),
                        options: Some(vec!["arrays".into(), "strings".into()]),
                        multi_select: None,
                    }),
                    QuestionItemOrString::Item(QuestionItem {
                        question: "Format?".into(),
                        options: Some(vec!["coding challenge".into(), "quiz".into()]),
                        multi_select: None,
                    }),
                ]),
            },
        )
        .await
        .unwrap();

        assert!(res.contains("1. Language?: Python"));
        assert!(res.contains("2. Topic?: arrays"));
        assert!(res.contains("3. Format?: coding challenge"));
    }

    #[test]
    fn an_option_written_as_an_object_is_shown_as_words_not_json() {
        let parsed: QuestionItem = serde_json::from_str(
            r#"{"question":"Which?","options":[{"label":"Fix the test","description":"edit the expectation"},{"old_string":"a","new_string":"b"},"Skip"]}"#,
        )
        .unwrap();
        assert_eq!(
            parsed.options,
            Some(vec!["Fix the test \u{2014} edit the expectation".to_string(), "new_string: b; old_string: a".to_string(), "Skip".to_string()])
        );
    }

    #[test]
    fn ask_user_deserializes_comma_options_and_aliases() {
        let json = r#"{"prompt": "Pick a tool", "choices": "git, cargo, rustc"}"#;
        let parsed: AskUserArgs = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.question.as_deref(), Some("Pick a tool"));
        let claude: AskUserArgs = serde_json::from_str(
            r#"{"questions":[{"question":"Which database?","header":"Database","options":["Postgres","SQLite"]},{"header":"Only a header"}]}"#,
        )
        .expect("a header beside the question is not a duplicate field");
        let questions: Vec<String> = claude.questions.unwrap().into_iter().map(|q| match q {
            QuestionItemOrString::Item(item) => item.question,
            QuestionItemOrString::Simple(s) => s,
        }).collect();
        assert_eq!(questions, vec!["Which database?", "Only a header"]);
        assert_eq!(
            parsed.options,
            Some(vec!["git".into(), "cargo".into(), "rustc".into()])
        );
    }
}
