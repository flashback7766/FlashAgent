use super::*;

/// Extract the current stage title from streamed reasoning. Models can indicate
/// their stage using markers like `[Stage: Name]`, `### Name`, or numbered steps
/// (`1. Analyze the Request:`, `1.  **Analyze the Request:**`, `5.  **Final Output Generation.**`).
/// The collapsed preview displays the LATEST stage.
pub fn reasoning_stage(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    for l in text.lines() {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }

        // 1. Explicit bracketed stage marker: `[Stage: Name]`, `[Phase: Name]`, `[Step: Name]`, `[Stage Name]`
        if let Some(rest) = t
            .strip_prefix("[Stage:")
            .or_else(|| t.strip_prefix("[stage:"))
            .or_else(|| t.strip_prefix("[Phase:"))
            .or_else(|| t.strip_prefix("[Step:"))
            .or_else(|| t.strip_prefix("[Stage "))
        {
            if let Some((name, _)) = rest.split_once(']') {
                let name = name.trim();
                if !name.is_empty() && name.chars().count() <= 60 {
                    last = Some(name.to_string());
                    continue;
                }
            }
        }

        // 2. Markdown heading: `### Name` or `## Name` or `# Name`
        if let Some(heading) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")).or_else(|| t.strip_prefix("# ")) {
            let name = heading.trim().trim_matches('#').trim().trim_matches('*').trim();
            if !name.is_empty() && name.chars().count() <= 60 {
                last = Some(name.to_string());
                continue;
            }
        }

        // 3. Numbered or bulleted labeled steps: `N. Title:` or `* Title:` or `* **Title:**` or `N.  **Title**`
        let step = if let Some(body) = t.strip_prefix("* ").or_else(|| t.strip_prefix("- ")) {
            Some(body)
        } else {
            t.split_once('.').and_then(|(n, rest)| {
                let n_trim = n.trim();
                (!n_trim.is_empty() && n_trim.chars().all(|c| c.is_ascii_digit())).then_some(rest.trim_start())
            })
        };
        if let Some(body) = step {
            let body = body.trim();
            if let Some((title, _)) = body.split_once(':') {
                let title = title.trim().trim_matches('*').trim();
                if !title.is_empty() && title.chars().count() <= 60 {
                    last = Some(title.to_string());
                    continue;
                }
            }
            // Also handle `N. **Title**` (without trailing colon, e.g. `5.  **Final Output Generation.**`)
            if let Some(stripped) = body.strip_prefix("**") {
                if let Some(end) = stripped.find("**") {
                    let title = stripped[..end].trim().trim_end_matches('.').trim();
                    if !title.is_empty() && title.chars().count() <= 60 {
                        last = Some(title.to_string());
                        continue;
                    }
                }
            }
        }

        // 4. Standalone bold header or stage title: `**Stage Title**` or `**Step 1: ...**`
        if let Some(stripped) = t.strip_prefix("**") {
            if let Some(end) = stripped.find("**") {
                let inner = stripped[..end].trim().trim_end_matches(':').trim();
                if inner.chars().count() >= 3 && inner.chars().count() <= 60 && !inner.contains('\n') {
                    last = Some(inner.to_string());
                    continue;
                }
            }
        }

        // 5. Unbracketed Step or Phase: `Step 1: ...` or `Phase 1: ...`
        if let Some(rest) = t.strip_prefix("Step ").or_else(|| t.strip_prefix("Phase ")) {
            if let Some((_num, title)) = rest.split_once(':') {
                let clean = title.trim().trim_matches('*').trim();
                if clean.chars().count() >= 3 && clean.chars().count() <= 60 && !clean.contains('\n') {
                    last = Some(clean.to_string());
                    continue;
                }
            }
        }
    }
    if last.is_none() {
        let lower = text.to_lowercase();
        // 1. Specific verification / builds
        if lower.contains("cargo test") || lower.contains("cargo build") || lower.contains("cargo check") || lower.contains("cargo clippy") {
            return Some("Planning Verification".to_string());
        }
        if lower.contains("read memory") || lower.contains("read the memory") || lower.contains("reading memory") {
            return Some("Reading memory files".to_string());
        }
        if lower.contains("agents.md") || lower.contains("project rules") || lower.contains("philosophy.md") {
            return Some("Checking project rules".to_string());
        }

        // 2. High-priority tool actions: search, files, edits, shell
        if lower.contains("search result") || lower.contains("grep found") || lower.contains("glob found") || lower.contains("found definition") {
            return Some("Evaluating Search Results".to_string());
        }
        if lower.contains("read file") || lower.contains("file content") || lower.contains("in this file") || lower.contains("looking at the code") {
            return Some("Analyzing File Contents".to_string());
        }
        if lower.contains("edit file") || lower.contains("write file") || lower.contains("patch file") || lower.contains("applying edit") {
            return Some("Planning Code Changes".to_string());
        }
        if lower.contains("run shell") || lower.contains("running command") {
            return Some("Planning Command Execution".to_string());
        }
        if lower.contains("evaluat") || lower.contains("tool result") || lower.contains("tool output") {
            return Some("Evaluating Tool Results".to_string());
        }
        if lower.contains("plan") || lower.contains("next step") || lower.contains("next action") {
            return Some("Planning Next Action".to_string());
        }
        if lower.contains("final") || lower.contains("answer") || lower.contains("solution") || lower.contains("respond to") {
            return Some("Formulating Response".to_string());
        }
        if lower.contains("respond in english") || lower.contains("respond in text") {
            return Some("Formulating English response".to_string());
        }

        // 3. Initial request understanding (placed after specific tool/code actions)
        if lower.contains("user said") || lower.contains("user wants") || lower.contains("user asked") {
            return Some("Understanding user request".to_string());
        }
    }
    last
}

/// Helper to derive an intuitive stage name from the most recently executed tool.
pub fn derive_stage_from_tool(last_tool_group: Option<&ToolGroupKind>) -> String {
    match last_tool_group {
        Some(ToolGroupKind::Explore { files, searches, .. }) => {
            if *searches > 0 && *files == 0 {
                "Evaluating Search Results".to_string()
            } else if *files > 0 && *searches == 0 {
                "Analyzing File Contents".to_string()
            } else {
                "Evaluating Explored Context".to_string()
            }
        }
        Some(ToolGroupKind::Command { .. }) => "Evaluating Command Output".to_string(),
        Some(ToolGroupKind::Edit { .. }) => "Verifying Code Changes".to_string(),
        Some(ToolGroupKind::Subagent { .. }) => "Evaluating Subagent Output".to_string(),
        Some(ToolGroupKind::Memory { .. }) => "Reviewing Project Memory".to_string(),
        _ => "Evaluating Tool Results".to_string(),
    }
}

/// Say one of the stage labels this app writes itself in the language of the
/// conversation.
///
/// Only labels FlashAgent invents are translated. A stage lifted out of the
/// model's own reasoning is the model's wording and is left exactly as it
/// wrote it — putting Russian words in its mouth would be a lie about what it
/// said.
pub fn localize_stage(stage: &str, language: &str) -> String {
    if !language.eq_ignore_ascii_case("ru") {
        return stage.to_string();
    }
    const RU: &[(&str, &str)] = &[
        ("Analyzing Request", "Разбираю запрос"),
        ("Evaluating Search Results", "Оцениваю результаты поиска"),
        ("Deepening Code Search", "Углубляю поиск по коду"),
        ("Analyzing File Contents", "Разбираю содержимое файлов"),
        ("Evaluating Explored Context", "Оцениваю найденное"),
        ("Evaluating Command Output", "Оцениваю вывод команды"),
        ("Verifying Code Changes", "Проверяю правки"),
        ("Evaluating Subagent Output", "Оцениваю ответ субагента"),
        ("Reviewing Project Memory", "Просматриваю память проекта"),
        ("Evaluating Tool Results", "Оцениваю результаты инструментов"),
        ("Synthesizing Findings", "Свожу выводы"),
        ("Planning Implementation", "Планирую реализацию"),
        ("Refining Solution", "Уточняю решение"),
        ("Verifying Solution", "Проверяю решение"),
        ("Formulating Response", "Формулирую ответ"),
    ];
    // A repeated stage comes back as "Name (part 2)"; the suffix is ours too.
    let (base, part) = match stage.split_once(" (part ") {
        Some((base, rest)) => (base, rest.trim_end_matches(')').parse::<u32>().ok()),
        None => (stage, None),
    };
    let translated = RU
        .iter()
        .find(|(en, _)| en.eq_ignore_ascii_case(base))
        .map(|(_, ru)| (*ru).to_string());
    match (translated, part) {
        (Some(ru), Some(n)) => format!("{ru} (часть {n})"),
        (Some(ru), None) => ru,
        (None, _) => stage.to_string(),
    }
}

/// Contextual reasoning stage resolver.
/// Determines a non-repeating, progress-advancing stage label for a reasoning block
/// based on the text, what tools have already run in the current turn, and what stages
/// were previously assigned in the same turn.
pub fn resolve_reasoning_stage(
    text: &str,
    prior_stages: &[String],
    last_tool_group: Option<&ToolGroupKind>,
    tools_executed: usize,
) -> String {
    let raw_stage = reasoning_stage(text);

    let is_initial_like = |s: &str| {
        let lower = s.to_lowercase();
        lower.contains("understanding")
            || lower.contains("analyzing request")
            || lower.contains("analyze the request")
            || lower.contains("determine the goal")
            || lower.contains("identify the goal")
    };

    let stage = if let Some(parsed) = raw_stage {
        if tools_executed > 0 && is_initial_like(&parsed) {
            derive_stage_from_tool(last_tool_group)
        } else {
            parsed
        }
    } else if tools_executed == 0 {
        "Analyzing Request".to_string()
    } else {
        derive_stage_from_tool(last_tool_group)
    };

    if prior_stages.iter().any(|p| p.eq_ignore_ascii_case(&stage)) {
        let candidates = [
            "Evaluating Search Results",
            "Deepening Code Search",
            "Analyzing File Contents",
            "Synthesizing Findings",
            "Planning Implementation",
            "Refining Solution",
            "Verifying Solution",
            "Formulating Response",
        ];

        for candidate in candidates {
            if !prior_stages.iter().any(|p| p.eq_ignore_ascii_case(candidate)) {
                return candidate.to_string();
            }
        }

        let count = prior_stages.iter().filter(|p| p.to_lowercase().starts_with(&stage.to_lowercase())).count();
        format!("{stage} (part {})", count + 1)
    } else {
        stage
    }
}

/// Extracts embedded thinking/scratchpad from assistant text, returning
/// `(Some(thinking), remaining_answer)` or `(None, original_text)` if no thinking is found.
pub fn extract_thinking_from_text(text: &str) -> (Option<String>, String) {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            let think = rest[..end].trim().to_string();
            let after = rest[end + "</think>".len()..].trim_start().to_string();
            return (Some(think), after);
        } else {
            let think = rest.trim().to_string();
            return (Some(think), String::new());
        }
    }

    if t.starts_with("Thinking Process:")
        || t.starts_with("Thinking:\n")
        || t.starts_with("[Stage:")
        || t.starts_with("### Stage:")
    {
        let mut thinking_lines = Vec::new();
        let mut answer_lines = Vec::new();
        let mut in_thinking = true;

        for line in text.lines() {
            let trimmed = line.trim();
            if in_thinking {
                if trimmed.is_empty()
                    || trimmed.starts_with("Thinking Process:")
                    || trimmed.starts_with("Thinking:")
                    || is_step_line(trimmed)
                {
                    thinking_lines.push(line);
                } else {
                    in_thinking = false;
                    answer_lines.push(line);
                }
            } else {
                answer_lines.push(line);
            }
        }

        let think_str = thinking_lines.join("\n").trim().to_string();
        let ans_str = answer_lines.join("\n").trim().to_string();
        if !think_str.is_empty() {
            return (Some(think_str), ans_str);
        }
    }

    (None, text.to_string())
}

pub(crate) fn is_step_line(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with("[Stage:") || t.starts_with("[stage:") || t.starts_with("[Phase:") || t.starts_with("[Step:") {
        return t.contains(']');
    }
    if t.starts_with("### ") || t.starts_with("## ") {
        return true;
    }
    if t.starts_with("**") && t.ends_with("**") && t.len() > 4 {
        return true;
    }
    if let Some(body) = t.strip_prefix("* ").or_else(|| t.strip_prefix("- ")) {
        body.contains(':')
    } else {
        t.split_once(". ").is_some_and(|(n, rest)| {
            !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && rest.contains(':')
        })
    }
}
