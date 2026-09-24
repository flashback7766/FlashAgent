use super::*;

/// Once, right after setup.
pub(crate) async fn first_run_tool_check(config: &AppConfig) -> Option<String> {
    // Without a server every probe fails, and the verdict would blame the model.
    let url = config.backend_url.trim_end_matches('/');
    let probe = flashagent_llm::Client::new(config.endpoint(), "");
    if config.model.trim().is_empty() || probe.discover_server().await.is_none() {
        return Some(format!(
            "Tool-calling check skipped: {url} did not list any models. Run `flashagent --tool-test` once it does."
        ));
    }
    println!("\nChecking whether {} can drive tools…", config.model);
    let source = BackendSource(flashagent_llm::Client::new(config.endpoint(), &config.model));
    let report = flashagent_core::toolcheck::check_model(
        &source,
        &config.model,
        std::time::Duration::from_secs(60),
    )
    .await;
    for line in report.lines() {
        println!("{line}");
    }
    let (passed, total) = report.score();
    if passed < total {
        println!(
            "\nA model that fails these will talk about doing the work instead of doing it.\n\
             You can re-run this any time with `flashagent --tool-test`, or compare models\n\
             with `flashagent --tool-test --all-models`."
        );
    }
    println!();

    // Printed output was cleared by the app starting; the verdict goes into the
    // conversation instead.
    Some(format!(
        "Tool-calling check: {passed}/{total} — {}. Re-run with `flashagent --tool-test`.",
        report.verdict()
    ))
}

/// `--tool-test`, for the configured model or every listed one. Exits non-zero
/// when a model cannot drive tools, so it works as a check.
pub(crate) async fn run_tool_check_cli(config: &AppConfig, all_models: bool) -> i32 {
    let timeout = std::time::Duration::from_secs(120);
    let mut models = vec![config.model.clone()];
    if all_models {
        let probe = BackendSource(flashagent_llm::Client::new(config.endpoint(), &config.model));
        match probe.discover_server().await {
            Some(disc) if !disc.models.is_empty() => {
                models = disc
                    .models
                    .iter()
                    .map(|m| m.id.clone())
                    .filter(|id| flashagent_core::toolcheck::is_chat_model(id))
                    .collect();
            }
            _ => {
                eprintln!("Could not list models at {}; checking the configured one only.", config.backend_url);
            }
        }
    }

    println!("Tool-calling check against {}", config.backend_url);
    let mut reports = Vec::new();
    for model in &models {
        println!("\n{model}");
        let source = BackendSource(flashagent_llm::Client::new(config.endpoint(), model));
        let report = flashagent_core::toolcheck::check_model(&source, model, timeout).await;
        for line in report.lines() {
            println!("{line}");
        }
        reports.push(report);
    }

    if reports.len() > 1 {
        println!("\n{}", flashagent_core::toolcheck::MARKDOWN_HEADER);
        for r in &reports {
            println!("{}", r.markdown_row());
        }
    }
    let json: Vec<_> = reports.iter().map(|r| r.to_json()).collect();
    if let Ok(path) = std::env::var("FLASHAGENT_TOOL_TEST_JSON") {
        // Raw results, so a published table can be checked.
        if let Ok(text) = serde_json::to_string_pretty(&json) {
            let _ = std::fs::write(&path, text);
            println!("\nRaw results written to {path}");
        }
    }

    i32::from(reports.iter().any(|r| r.score().0 == 0))
}

/// Settings → "Run Tool Test": a well-formed call, native or recovered from text.
pub(crate) async fn run_tool_call_probe(source: &BackendSource) -> String {
    use futures::StreamExt;
    let probe = flashagent_llm::ToolSpec {
        name: "report_status".into(),
        description: "Report a numeric status code back to the test harness.".into(),
        parameters_json: r#"{"type":"object","properties":{"code":{"type":"integer"}},"required":["code"]}"#.into(),
    };
    let messages = vec![
        ChatMessage::system("You are a tool-calling test harness. Respond only by calling the provided tool."),
        ChatMessage::user("Call the report_status tool with code 42."),
    ];
    let opts = flashagent_llm::TurnOptions {
        thinking: flashagent_llm::ThinkingEffort::Off,
        temperature: Some(0.0),
        max_tokens: Some(1024),
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let run = async {
        let mut stream = source.turn_with_options(&messages, std::slice::from_ref(&probe), &opts).await.map_err(|e| e.to_string())?;
        let (mut name, mut args, mut text) = (String::new(), String::new(), String::new());
        while let Some(ev) = stream.next().await {
            match ev.map_err(|e| e.to_string())? {
                flashagent_llm::LlmEvent::ToolCallDelta { name: Some(n), args_delta, .. } => {
                    name = n;
                    args.push_str(&args_delta);
                }
                flashagent_llm::LlmEvent::ToolCallDelta { args_delta, .. } => args.push_str(&args_delta),
                flashagent_llm::LlmEvent::TextDelta(t) => text.push_str(&t),
                _ => {}
            }
        }
        Ok::<_, String>((name, args, text))
    };
    let (name, args, text) = match tokio::time::timeout(std::time::Duration::from_secs(120), run).await {
        Err(_) => return "FAIL: no answer within 120s".into(),
        Ok(Err(e)) => return format!("FAIL: {e}"),
        Ok(Ok(parts)) => parts,
    };
    let secs = started.elapsed().as_secs_f32();
    let (name, args, via) = if !name.is_empty() {
        (name, args, "native")
    } else {
        let mut scanner = flashagent_llm::TextToolScanner::default();
        let mut events = scanner.feed(&text);
        events.extend(scanner.finish());
        match events.into_iter().find_map(|e| match e {
            flashagent_llm::ScannerEvent::ToolCall { name, args_json, .. } => Some((name, args_json)),
            flashagent_llm::ScannerEvent::Text(_) => None,
        }) {
            Some((n, a)) => (n, a, "text markup"),
            None => {
                let snippet: String = text.trim().chars().take(60).collect();
                return format!("FAIL: model replied with text, no tool call ({snippet:?})");
            }
        }
    };
    let code = serde_json::from_str::<serde_json::Value>(&args)
        .ok()
        .or_else(|| flashagent_llm::repair_json(&args).and_then(|r| serde_json::from_str(&r).ok()))
        .and_then(|v| v.get("code").and_then(|c| c.as_i64()));
    match (name == "report_status", code) {
        (true, Some(42)) => format!("PASS: {via} tool call in {secs:.1}s"),
        (true, _) => format!("PARTIAL: {via} call, wrong arguments {args}"),
        (false, _) => format!("FAIL: called unknown tool '{name}'"),
    }
}
