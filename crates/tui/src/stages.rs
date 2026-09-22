use super::*;

/// Markers: `[Stage: Name]`, `### Name`, or numbered steps (`1. Analyze the
/// Request:`, `5.  **Final Output Generation.**`). The latest stage wins.
pub fn reasoning_stage(text: &str) -> Option<String> {
    let mut last: Option<String> = None;
    for l in text.lines() {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }

        // 1. `[Stage: Name]`, `[Phase: Name]`, `[Step: Name]`, `[Stage Name]`
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

        // 2. Markdown heading.
        if let Some(heading) = t.strip_prefix("### ").or_else(|| t.strip_prefix("## ")).or_else(|| t.strip_prefix("# ")) {
            let name = heading.trim().trim_matches('#').trim().trim_matches('*').trim();
            if !name.is_empty() && name.chars().count() <= 60 {
                last = Some(name.to_string());
                continue;
            }
        }

        // 3. `N. Title:`, `* Title:`, `* **Title:**`, `N.  **Title**`
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
            // `N. **Title**` without a trailing colon.
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

        // 4. `**Stage Title**` or `**Step 1: ...**`
        if let Some(stripped) = t.strip_prefix("**") {
            if let Some(end) = stripped.find("**") {
                let inner = stripped[..end].trim().trim_end_matches(':').trim();
                if inner.chars().count() >= 3 && inner.chars().count() <= 60 && !inner.contains('\n') {
                    last = Some(inner.to_string());
                    continue;
                }
            }
        }

        // 5. `Step 1: ...` or `Phase 1: ...`
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
        if lower.contains("cargo test") || lower.contains("cargo build") || lower.contains("cargo check") || lower.contains("cargo clippy") {
            return Some("Planning Verification".to_string());
        }
        if lower.contains("read memory") || lower.contains("read the memory") || lower.contains("reading memory") {
            return Some("Reading memory files".to_string());
        }
        if lower.contains("agents.md") || lower.contains("project rules") || lower.contains("philosophy.md") {
            return Some("Checking project rules".to_string());
        }

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

        // After the specific tool actions above on purpose.
        if lower.contains("user said") || lower.contains("user wants") || lower.contains("user asked") {
            return Some("Understanding user request".to_string());
        }
    }
    last
}

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

/// A non-repeating, progress-advancing label, from the text, the tools already
/// run this turn and the stages already assigned.
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

/// `(Some(thinking), answer)`, or `(None, original)` when there is none.
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

    // A model without a reasoning channel may narrate thinking as this project's
    // prompt asks (REASONING INSTRUCTIONS in prompt.rs): paragraphs opening with a
    // bold stage title. Thinking runs through every such paragraph and stops at
    // the first that is not one. Requiring the known vocabulary and at least two
    // in a row keeps an answer's own `**Summary**` heading from being swallowed.
    let blocks: Vec<&str> = text.split("\n\n").map(str::trim).filter(|b| !b.is_empty()).collect();
    let mut split_at = 0;
    for block in &blocks {
        let first_line = block.lines().next().unwrap_or("").trim();
        if looks_like_a_stage_title(first_line) {
            split_at += 1;
        } else {
            break;
        }
    }
    if split_at >= 2 {
        let think_str = blocks[..split_at].join("\n\n").trim().to_string();
        let ans_str = blocks[split_at..].join("\n\n").trim().to_string();
        if !think_str.is_empty() {
            return (Some(think_str), ans_str);
        }
    }

    (None, text.to_string())
}

/// From the stage titles the prompt asks for. A line needs both a step shape
/// and one of these words, so a real answer's heading does not qualify.
const STAGE_KEYWORDS: &[&str] = &[
    "understanding", "analyz", "evaluat", "inspecting", "planning", "formulating", "synthesizing",
    "deepening", "refining", "verifying",
];

fn looks_like_a_stage_title(line: &str) -> bool {
    is_step_line(line) && {
        let lower = line.to_lowercase();
        STAGE_KEYWORDS.iter().any(|k| lower.contains(k))
    }
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
