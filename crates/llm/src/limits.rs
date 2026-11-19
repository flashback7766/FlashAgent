//! Account balance and plan limits, where a provider publishes them for the
//! key the owner already configured.
//!
//! Not every API has such an endpoint, and inventing one from a price table is
//! worse than saying it is missing. Each family below is a known endpoint with
//! a known shape: DeepSeek's balance, OpenRouter's credits and key limits, and
//! the `dashboard/billing` pair that new-api/one-api gateways (AgentRouter and
//! friends) expose. Everything else is reported as unsupported, by name, so the
//! menu can say why a row is empty instead of showing a zero.

use std::time::Duration;

use crate::protocol::Endpoint;

#[derive(Debug, Clone, PartialEq)]
pub struct LimitLine {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LimitsOutcome {
    /// What the provider said. Lines are shown in order.
    Reported(Vec<LimitLine>),
    /// This API has no endpoint a normal key may ask (most of them).
    Unsupported(&'static str),
    /// The endpoint exists but did not answer, or answered something else.
    Failed(String),
}

/// One balance check, bounded: a menu must open even when the provider is
/// slow, and a failed check is a line, not a reason to keep the spinner.
pub async fn fetch(endpoint: &Endpoint) -> LimitsOutcome {
    let host = host_of(&endpoint.url);
    if is_local(&host) {
        return LimitsOutcome::Unsupported("this server runs on your machine; there is no account to ask");
    }
    let task = async {
        if host.ends_with("deepseek.com") {
            return deepseek(endpoint).await;
        }
        if host.ends_with("openrouter.ai") {
            return openrouter(endpoint).await;
        }
        if host.ends_with("opencode.ai") {
            return LimitsOutcome::Unsupported(
                "the plan counts a per-turn allowance, not an account balance: it is shown in Session",
            );
        }
        if host.ends_with("openai.com") || host.ends_with("anthropic.com") || host.ends_with("googleapis.com") {
            return LimitsOutcome::Unsupported("this API does not expose an account balance to a normal key");
        }
        // A gateway may be a new-api/one-api deployment: those publish the
        // dashboard pair under /v1. A 404 is the honest "not here".
        gateway(endpoint).await
    };
    tokio::time::timeout(Duration::from_secs(8), task)
        .await
        .unwrap_or_else(|_| LimitsOutcome::Failed("the provider did not answer in 8 seconds".to_string()))
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host_port.strip_prefix('[') {
        Some(inner) => inner.split(']').next().unwrap_or(inner),
        None => host_port.split(':').next().unwrap_or(host_port),
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

fn origin_of(url: &str) -> String {
    let scheme = url.split_once("://").map_or("https", |(scheme, _)| scheme);
    format!("{scheme}://{}", host_of(url))
}

fn is_local(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || v6.is_unspecified(),
        };
    }
    false
}

fn client(endpoint: &Endpoint) -> Result<reqwest::Client, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(key) = endpoint.api_key.as_deref().filter(|k| !k.trim().is_empty()) {
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {}", key.trim()))
            .map_err(|_| "the provider key cannot be sent as a header".to_string())?;
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    let ua = endpoint.user_agent.clone().unwrap_or_else(|| "flashagent".to_string());
    reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .default_headers(headers)
        .user_agent(ua)
        .build()
        .map_err(|e| e.to_string())
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<serde_json::Value, String> {
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    serde_json::from_str(&text).map_err(|_| "the answer was not JSON".to_string())
}

fn money(amount: f64) -> String {
    if amount > 0.0 && amount < 0.01 {
        format!("${amount:.4}")
    } else {
        format!("${amount:.2}")
    }
}

fn number(value: &serde_json::Value, pointer: &str) -> Option<f64> {
    value.pointer(pointer).and_then(serde_json::Value::as_f64).filter(|n| n.is_finite())
}

async fn deepseek(endpoint: &Endpoint) -> LimitsOutcome {
    let client = match client(endpoint) {
        Ok(c) => c,
        Err(e) => return LimitsOutcome::Failed(e),
    };
    let url = format!("{}/user/balance", origin_of(&endpoint.url));
    match get_json(&client, &url).await {
        Ok(value) => deepseek_lines(&value).map_or_else(
            || LimitsOutcome::Failed("DeepSeek answered without a balance".to_string()),
            LimitsOutcome::Reported,
        ),
        Err(e) => LimitsOutcome::Failed(format!("DeepSeek balance: {e}")),
    }
}

fn deepseek_lines(value: &serde_json::Value) -> Option<Vec<LimitLine>> {
    let entries = value.get("balance_infos").and_then(serde_json::Value::as_array)?;
    let mut lines = Vec::new();
    if let Some(available) = value.get("is_available").and_then(serde_json::Value::as_bool) {
        lines.push(LimitLine { label: "Available".into(), value: if available { "yes".into() } else { "no".into() } });
    }
    for entry in entries {
        let currency = entry.get("currency").and_then(serde_json::Value::as_str).unwrap_or("CNY");
        let total = entry.get("total_balance").and_then(serde_json::Value::as_str).and_then(|s| s.parse::<f64>().ok());
        let granted = entry.get("granted_balance").and_then(serde_json::Value::as_str).and_then(|s| s.parse::<f64>().ok());
        let topped = entry.get("topped_up_balance").and_then(serde_json::Value::as_str).and_then(|s| s.parse::<f64>().ok());
        if let Some(total) = total {
            lines.push(LimitLine { label: format!("Balance ({currency})"), value: format!("{total:.2}") });
        }
        if granted.is_some() || topped.is_some() {
            lines.push(LimitLine {
                label: format!("Granted / paid ({currency})"),
                value: format!("{:.2} / {:.2}", granted.unwrap_or(0.0), topped.unwrap_or(0.0)),
            });
        }
    }
    (!lines.is_empty()).then_some(lines)
}

async fn openrouter(endpoint: &Endpoint) -> LimitsOutcome {
    let client = match client(endpoint) {
        Ok(c) => c,
        Err(e) => return LimitsOutcome::Failed(e),
    };
    let origin = origin_of(&endpoint.url);
    let credits = get_json(&client, &format!("{origin}/api/v1/credits")).await.ok();
    let key = get_json(&client, &format!("{origin}/api/v1/key")).await.ok();
    if credits.is_none() && key.is_none() {
        return LimitsOutcome::Failed("OpenRouter did not answer /credits or /key".to_string());
    }
    match openrouter_lines(credits.as_ref(), key.as_ref()) {
        lines if !lines.is_empty() => LimitsOutcome::Reported(lines),
        _ => LimitsOutcome::Failed("OpenRouter answered without limits".to_string()),
    }
}

fn openrouter_lines(credits: Option<&serde_json::Value>, key: Option<&serde_json::Value>) -> Vec<LimitLine> {
    let mut lines = Vec::new();
    if let Some(credits) = credits {
        if let (Some(total), Some(used)) = (number(credits, "/data/total_credits"), number(credits, "/data/total_usage")) {
            lines.push(LimitLine { label: "Credits".into(), value: format!("{} of {} used", money(used), money(total)) });
            lines.push(LimitLine { label: "Left".into(), value: money((total - used).max(0.0)) });
        }
    }
    if let Some(key) = key {
        let data = key.get("data").unwrap_or(key);
        if let Some(label) = data.get("label").and_then(serde_json::Value::as_str).filter(|l| !l.is_empty()) {
            lines.push(LimitLine { label: "Key".into(), value: label.to_string() });
        }
        if let Some(limit) = number(data, "/limit") {
            let used = number(data, "/usage").unwrap_or(0.0);
            let left = number(data, "/limit_remaining").unwrap_or((limit - used).max(0.0));
            lines.push(LimitLine {
                label: "Key limit".into(),
                value: format!("{} of {} used · {} left", money(used), money(limit), money(left)),
            });
        } else if let Some(used) = number(data, "/usage") {
            lines.push(LimitLine { label: "Key usage".into(), value: money(used) });
        }
        if data.get("is_free_tier").and_then(serde_json::Value::as_bool) == Some(true) {
            lines.push(LimitLine { label: "Tier".into(), value: "free".into() });
        }
        if let Some(rpm) = number(data, "/rate_limit/requests_per_minute") {
            lines.push(LimitLine { label: "Rate limit".into(), value: format!("{rpm:.0} requests/min") });
        }
    }
    lines
}

async fn gateway(endpoint: &Endpoint) -> LimitsOutcome {
    let client = match client(endpoint) {
        Ok(c) => c,
        Err(e) => return LimitsOutcome::Failed(e),
    };
    let origin = origin_of(&endpoint.url);
    let subscription = format!("{origin}/v1/dashboard/billing/subscription");
    let value = match get_json(&client, &subscription).await {
        Ok(value) => value,
        Err(_) => {
            return LimitsOutcome::Unsupported(
                "this gateway does not publish a balance endpoint for an API key",
            )
        }
    };
    let mut lines = Vec::new();
    if let Some(hard) = number(&value, "/hard_limit_usd").or_else(|| number(&value, "/system_hard_limit_usd")) {
        lines.push(LimitLine { label: "Monthly limit".into(), value: money(hard) });
    }
    if let Some(soft) = number(&value, "/soft_limit_usd") {
        lines.push(LimitLine { label: "Soft limit".into(), value: money(soft) });
    }
    // The usage figure lives in a second call, in cents, over a date range the
    // gateway insists on. Last 30 days is what its console shows by default.
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let day = |shift: u64| limits_day(now.saturating_sub(shift));
    let usage_url = format!(
        "{origin}/v1/dashboard/billing/usage?start_date={}&end_date={}",
        day(30 * 86_400),
        day(0)
    );
    if let Ok(usage) = get_json(&client, &usage_url).await {
        if let Some(cents) = number(&usage, "/total_usage") {
            lines.push(LimitLine { label: "Used (30 days)".into(), value: money(cents / 100.0) });
        }
    }
    if lines.is_empty() {
        return LimitsOutcome::Failed("the gateway answered without a limit".to_string());
    }
    LimitsOutcome::Reported(lines)
}

/// `YYYY-MM-DD` for the query string, UTC like the usage history.
fn limits_day(at: u64) -> String {
    let days = (at / 86_400) as i64;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_balance_is_read_the_way_deepseek_writes_it() {
        let value: serde_json::Value = serde_json::from_str(
            r#"{"is_available":true,"balance_infos":[{"currency":"CNY","total_balance":"12.34","granted_balance":"2.00","topped_up_balance":"10.34"}]}"#,
        )
        .unwrap();
        let lines = deepseek_lines(&value).unwrap();
        assert!(lines.iter().any(|l| l.label == "Balance (CNY)" && l.value == "12.34"));
        assert!(lines.iter().any(|l| l.value == "2.00 / 10.34"));
    }

    #[test]
    fn openrouter_credits_and_key_limits_are_one_list() {
        let credits: serde_json::Value = serde_json::from_str(r#"{"data":{"total_credits":10.0,"total_usage":2.5}}"#).unwrap();
        let key: serde_json::Value =
            serde_json::from_str(r#"{"data":{"label":"work","usage":2.5,"limit":5.0,"limit_remaining":2.5,"is_free_tier":false,"rate_limit":{"requests_per_minute":60}}}"#).unwrap();
        let lines = openrouter_lines(Some(&credits), Some(&key));
        assert!(lines.iter().any(|l| l.label == "Left" && l.value == "$7.50"));
        assert!(lines.iter().any(|l| l.label == "Key limit" && l.value.contains("$2.50 left")));
        assert!(lines.iter().any(|l| l.value == "60 requests/min"));
    }

    #[test]
    fn a_local_server_has_no_account_and_a_gateway_is_not_guessed() {
        assert!(is_local("localhost"));
        assert!(is_local("127.0.0.1"));
        assert!(is_local("192.168.1.5"));
        assert!(!is_local("api.example.com"));
        assert_eq!(host_of("https://user:pw@API.Example.com:8443/v1"), "api.example.com");
    }

    #[test]
    fn the_gateway_date_range_is_a_calendar_date() {
        assert_eq!(limits_day(1_700_000_000), "2023-11-14");
    }
}
