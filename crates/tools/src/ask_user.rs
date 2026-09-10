//! Interactive user questioning tool (`ask_user`).
//!
//! Enables the model to request guidance, clarification, or choices from the user.
//! In autonomous `/goal` mode, this tool is disabled to ensure independent execution.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use async_trait::async_trait;
use serde::Deserialize;

use crate::ToolError;

/// Async gate implemented by the UI/TUI to present interactive questions.
#[async_trait]
pub trait QuestionGate: Send + Sync {
    /// Prompt the user with a question and optional choices.
    /// Returns `(answer_string, is_write_in_flag)`.
    async fn ask(&self, question: &str, options: Option<&[String]>, multi_select: bool) -> Result<(String, bool), String>;
}

/// Flexible helper to deserialize either a list of strings or a comma-separated string.
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

/// A single question entry for multi-question mode or unified handling.
#[derive(Debug, Clone, Deserialize)]
pub struct QuestionItem {
    /// The question text.
    #[serde(alias = "prompt", alias = "title", alias = "text", alias = "header")]
    pub question: String,
    /// Optional selectable options (up to 10 choices).
    #[serde(default, deserialize_with = "deserialize_flexible_options", alias = "choices", alias = "items")]
    pub options: Option<Vec<String>>,
    /// Optional flag whether multiple options can be chosen simultaneously with checkboxes.
    #[serde(default, alias = "is_multi_select", alias = "multiple")]
    pub multi_select: Option<bool>,
}

/// Either a full `QuestionItem` object or a plain question string.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum QuestionItemOrString {
    Item(QuestionItem),
    Simple(String),
}

/// Arguments for `ask_user`.
#[derive(Debug, Clone, Deserialize)]
pub struct AskUserArgs {
    /// The question to present to the user (single question mode).
    #[serde(default, alias = "prompt", alias = "title", alias = "text")]
    pub question: Option<String>,
    /// Optional selectable options for single question mode.
    #[serde(default, deserialize_with = "deserialize_flexible_options", alias = "choices")]
    pub options: Option<Vec<String>>,
    /// Optional flag whether multiple options can be chosen simultaneously with checkboxes.
    #[serde(default, alias = "is_multi_select", alias = "multiple")]
    pub multi_select: Option<bool>,
    /// Sequential list of multiple questions to ask.
    #[serde(default, alias = "items")]
    pub questions: Option<Vec<QuestionItemOrString>>,
}

/// Executes the `ask_user` tool call.
pub async fn run_ask_user(
    gate: Option<&Arc<dyn QuestionGate>>,
    is_goal_mode: &Arc<AtomicBool>,
    args: AskUserArgs,
) -> Result<String, ToolError> {
    if is_goal_mode.load(Ordering::Relaxed) {
        return Err(ToolError::Other(
            "ask_user is disabled in autonomous /goal mode; you must operate independently without querying the user."
                .into(),
        ));
    }

    let gate = gate.ok_or_else(|| {
        ToolError::Other("interactive question gate is not available in this environment".into())
    })?;

    // Extract questions list
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

    // Sanitize options per question (relaxed bounds: 1..10, truncate if > 10)
    for item in &mut items {
        if let Some(ref mut opts) = item.options {
            if opts.is_empty() {
                item.options = None;
            } else if opts.len() > 10 {
                opts.truncate(10);
            }
        }
    }

    if items.len() == 1 {
        let item = &items[0];
        let question = item.question.trim();
        let multi_select = item.multi_select.unwrap_or(false);
        let (answer, is_write_in) = gate
            .ask(question, item.options.as_deref(), multi_select)
            .await
            .map_err(ToolError::Other)?;

        let answer_trimmed = answer.trim();
        let final_answer = if is_write_in {
            format!("{answer_trimmed} (write-in)")
        } else {
            answer_trimmed.to_string()
        };
        Ok(final_answer)
    } else {
        let mut results = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let question = item.question.trim();
            let multi_select = item.multi_select.unwrap_or(false);
            let (answer, is_write_in) = gate
                .ask(question, item.options.as_deref(), multi_select)
                .await
                .map_err(ToolError::Other)?;

            let answer_trimmed = answer.trim();
            let final_answer = if is_write_in {
                format!("{answer_trimmed} (write-in)")
            } else {
                answer_trimmed.to_string()
            };
            results.push(format!("{}. {}: {}", i + 1, question, final_answer));
        }
        Ok(results.join("\n"))
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
        async fn ask(&self, _question: &str, _options: Option<&[String]>, _multi_select: bool) -> Result<(String, bool), String> {
            let mut lock = self.responses.lock();
            if lock.is_empty() {
                Ok(("default".into(), false))
            } else {
                Ok(lock.remove(0))
            }
        }
    }

    #[tokio::test]
    async fn ask_user_goal_mode_is_blocked() {
        let gate: Arc<dyn QuestionGate> = Arc::new(MockGate::single(("Option 1".into(), false)));
        let goal_flag = Arc::new(AtomicBool::new(true));

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
        .await;

        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("disabled in autonomous /goal mode"));
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

        // Single option is allowed (1 option + write in)
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

        // More than 5 options (e.g. 7 options) are smoothly accepted and clamped
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
