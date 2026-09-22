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
                    res.push(item.to_string());
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
pub struct QuestionItem {
    #[serde(alias = "prompt", alias = "title", alias = "text", alias = "header")]
    pub question: String,
    /// Up to 10.
    #[serde(default, deserialize_with = "deserialize_flexible_options", alias = "choices", alias = "items")]
    pub options: Option<Vec<String>>,
    #[serde(default, alias = "is_multi_select", alias = "multiple")]
    pub multi_select: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum QuestionItemOrString {
    Item(QuestionItem),
    Simple(String),
}

#[derive(Debug, Clone, Deserialize)]
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
                Err(_) => return Ok(no_answer(&answered, goal_timeout)),
            },
            None => asked.await,
        };
        let (answer, is_write_in) = reply.map_err(ToolError::Other)?;

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
    let secs = waited.as_secs();
    let waited = if secs >= 60 && secs.is_multiple_of(60) { format!("{} minutes", secs / 60) } else { format!("{secs} seconds") };
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
    fn ask_user_deserializes_comma_options_and_aliases() {
        let json = r#"{"prompt": "Pick a tool", "choices": "git, cargo, rustc"}"#;
        let parsed: AskUserArgs = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.question.as_deref(), Some("Pick a tool"));
        assert_eq!(
            parsed.options,
            Some(vec!["git".into(), "cargo".into(), "rustc".into()])
        );
    }
}
