use super::*;

use flashagent_tui::memory_view::{fingerprint, parse_summary, summary_request, MemoryModal, MemorySummary, Row, SummaryState};

/// The last summary, so reopening is instant.
fn cache_path() -> Option<std::path::PathBuf> {
    flashagent_home_dir().map(|home| home.join("memory_summary.json"))
}

pub(crate) fn open_memory_modal() -> MemoryModal {
    let mut modal = MemoryModal::new(&std::env::current_dir().unwrap_or_default());
    if let Some(saved) = cache_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<MemorySummary>(&text).ok())
    {
        modal.summary = SummaryState::Ready(saved);
    }
    modal
}

pub(crate) fn save_summary(summary: &MemorySummary) {
    if let (Some(path), Ok(json)) = (cache_path(), serde_json::to_string_pretty(summary)) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, json);
    }
}

/// The answer arrives as `UiEvent::MemorySummary`.
pub(crate) fn spawn_summary(source: Arc<BackendSource>, rows: Vec<Row>, tx: tokio::sync::mpsc::UnboundedSender<UiEvent>) {
    tokio::spawn(async move {
        let bodies: String = rows.iter().map(|r| format!("{} {} ", r.entry.description, r.entry.body)).collect();
        let (system, user) = summary_request(&rows, script_language(&bodies));
        let messages = vec![ChatMessage::system(system), ChatMessage::user(user)];
        let opts = flashagent_llm::TurnOptions {
            thinking: flashagent_llm::ThinkingEffort::Off,
            custom_effort: Some("none".to_string()),
            temperature: Some(0.3),
            max_tokens: Some(2000),
            ..Default::default()
        };
        let task = async {
            use futures::StreamExt;
            let mut stream = source.turn_with_options(&messages, &[], &opts).await.map_err(|e| e.to_string())?;
            let (mut text, mut reasoning) = (String::new(), String::new());
            while let Some(ev) = stream.next().await {
                match ev {
                    Ok(flashagent_llm::LlmEvent::TextDelta(d)) => text.push_str(&d),
                    Ok(flashagent_llm::LlmEvent::ReasoningDelta(d)) => reasoning.push_str(&d),
                    Ok(flashagent_llm::LlmEvent::Done(_)) => break,
                    Ok(_) => {}
                    Err(e) => return Err(e.to_string()),
                }
            }
            let (sections, dive_deeper) = parse_summary(&text)
                .or_else(|| parse_summary(&reasoning))
                .ok_or_else(|| "the model did not answer with a summary".to_string())?;
            let updated = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            Ok(MemorySummary { sections, dive_deeper, updated, fingerprint: fingerprint(&rows) })
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), task)
            .await
            .unwrap_or_else(|_| Err("the model took longer than two minutes".to_string()));
        let _ = tx.send(UiEvent::MemorySummary(result));
    });
}
