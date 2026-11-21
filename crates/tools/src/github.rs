//! The GitHub issues tool: the room's memory of what is broken.
//!
//! The repository is fixed in the environment (`FLASHAGENT_GITHUB_REPO`),
//! the token too (`GITHUB_TOKEN`) — a model that could aim the tool at an
//! arbitrary repository is a model that could post anywhere. The tool talks
//! straight to the GitHub API; the mutation budget it shares with the
//! telegram tool is what keeps a confused turn from filing ten issues.

use serde::Deserialize;

use super::{parse_args, ToolError};

/// The connection a serve is built with when the environment names a repo.
/// `base_url` exists so tests can point the tool at a mock; production
/// always uses the public API.
pub struct GitHubConfig {
    pub token: String,
    pub repo: String,
    pub base_url: String,
}

impl GitHubConfig {
    pub fn from_env() -> Option<Self> {
        let token = std::env::var("GITHUB_TOKEN")
            .ok()
            .filter(|t| !t.trim().is_empty())?;
        let repo = std::env::var("FLASHAGENT_GITHUB_REPO")
            .ok()
            .filter(|r| !r.trim().is_empty())?;
        Some(Self {
            token: token.trim().to_string(),
            repo: repo.trim().to_string(),
            base_url: "https://api.github.com".into(),
        })
    }
}

#[derive(Deserialize)]
pub struct GitHubArgs {
    pub action: String,
    pub query: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub number: Option<i64>,
}

/// GitHub answers within seconds or the turn is stalling on it (H14).
const GITHUB_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

pub struct GitHub {
    http: reqwest::Client,
    cfg: GitHubConfig,
}

impl GitHub {
    pub fn new(cfg: GitHubConfig) -> Result<Self, ToolError> {
        let http = reqwest::Client::builder()
            .timeout(GITHUB_TIMEOUT)
            .connect_timeout(std::time::Duration::from_secs(5))
            .user_agent("FlashAgent")
            .build()
            .map_err(|e| ToolError::Other(format!("github client: {e}")))?;
        Ok(Self { http, cfg })
    }

    fn need<'a>(
        what: &str,
        v: &'a Option<String>,
        action: &str,
    ) -> Result<&'a str, ToolError> {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::Other(format!("err: {action} требует {what}")))
    }

    /// The `github` tool: guard, charge, call, answer with one short line.
    pub async fn run(&self, args_json: &str, charge: impl Fn() -> Result<(), ToolError>) -> Result<String, ToolError> {
        let a: GitHubArgs = parse_args(args_json, "github")?;
        let action = a.action.trim().to_ascii_lowercase();
        match action.as_str() {
            "search" => {
                let query = Self::need("query", &a.query, "search")?;
                self.search(query).await
            }
            "get" => {
                let number = a
                    .number
                    .ok_or_else(|| ToolError::Other("err: get требует number".into()))?;
                self.get(number).await
            }
            "create" => {
                let title = Self::need("title", &a.title, "create")?;
                let body = Self::need("body", &a.body, "create")?;
                charge()?;
                self.create(title, body).await
            }
            "comment" => {
                let number = a
                    .number
                    .ok_or_else(|| ToolError::Other("err: comment требует number".into()))?;
                let body = Self::need("body", &a.body, "comment")?;
                charge()?;
                self.comment(number, body).await
            }
            other => Err(ToolError::Other(format!(
                "err: неизвестное действие '{other}'"
            ))),
        }
    }

    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        json: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ToolError> {
        // Most endpoints hang off the repository; the search endpoint hangs
        // off the API root, so it passes a path starting with `!` and skips
        // the prefix.
        let url = match path.strip_prefix('!') {
            Some(rest) => format!("{}{}", self.cfg.base_url, rest),
            None => format!("{}/repos/{}{}", self.cfg.base_url, self.cfg.repo, path),
        };
        let mut req = self
            .http
            .request(method, &url)
            .bearer_auth(&self.cfg.token)
            .header("Accept", "application/vnd.github+json");
        if let Some(body) = json {
            req = req.json(&body);
        }
        let resp = req.send().await.map_err(|e| {
            ToolError::Other(format!("err: GitHub недоступен, повтори позже ({e})"))
        })?;
        let status = resp.status();
        let value: serde_json::Value = resp.json().await.map_err(|e| {
            ToolError::Other(format!("err: GitHub ответил не JSON (HTTP {status}) ({e})"))
        })?;
        if !status.is_success() {
            let message = value
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("без объяснения");
            return Err(ToolError::Other(format!(
                "err: GitHub отказал (HTTP {status}): {message}"
            )));
        }
        Ok(value)
    }

    async fn search(&self, query: &str) -> Result<String, ToolError> {
        // The repository qualifier is added here, so a model cannot widen
        // the search to someone else's issues.
        let full = format!("{query} repo:{}", self.cfg.repo);
        let value = self
            .call(
                reqwest::Method::GET,
                &format!("!/search/issues?q={}", urlencode(&full)),
                None,
            )
            .await?;
        let items = value
            .get("items")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        if items.is_empty() {
            return Ok("похоже, ничего такого не заведено — можно создавать новый".into());
        }
        let mut out = Vec::new();
        for item in items.iter().take(8) {
            let number = item.get("number").and_then(serde_json::Value::as_i64).unwrap_or(0);
            let state = item.get("state").and_then(serde_json::Value::as_str).unwrap_or("?");
            let title = item.get("title").and_then(serde_json::Value::as_str).unwrap_or("");
            out.push(format!("#{number} [{state}] {title}"));
        }
        Ok(format!("Найдено:\n{}", out.join("\n")))
    }

    async fn get(&self, number: i64) -> Result<String, ToolError> {
        let value = self.call(reqwest::Method::GET, &format!("/issues/{number}"), None).await?;
        let state = value.get("state").and_then(serde_json::Value::as_str).unwrap_or("?");
        let title = value.get("title").and_then(serde_json::Value::as_str).unwrap_or("");
        let body = value.get("body").and_then(serde_json::Value::as_str).unwrap_or("");
        Ok(format!("#{number} [{state}] {title}\n{}", body.chars().take(1500).collect::<String>()))
    }

    async fn create(&self, title: &str, body: &str) -> Result<String, ToolError> {
        let value = self
            .call(
                reqwest::Method::POST,
                "/issues",
                Some(serde_json::json!({ "title": title, "body": body })),
            )
            .await?;
        let number = value.get("number").and_then(serde_json::Value::as_i64).unwrap_or(0);
        let url = value.get("html_url").and_then(serde_json::Value::as_str).unwrap_or("");
        Ok(format!("ok: заведён issue #{number} {url}"))
    }

    async fn comment(&self, number: i64, body: &str) -> Result<String, ToolError> {
        self.call(
            reqwest::Method::POST,
            &format!("/issues/{number}/comments"),
            Some(serde_json::json!({ "body": body })),
        )
        .await?;
        Ok(format!("ok: комментарий к #{number} добавлен"))
    }
}

fn serde_urlencoded(params: &[(&str, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gh() -> GitHub {
        GitHub::new(GitHubConfig {
            token: "t".into(),
            repo: "own/repo".into(),
            base_url: "http://127.0.0.1:1".into(),
        })
        .unwrap()
    }

    async fn call(args: serde_json::Value) -> Result<String, ToolError> {
        gh().run(&args.to_string(), || Ok(())).await
    }

    #[tokio::test]
    async fn create_without_a_body_is_refused_before_the_network() {
        let err = call(serde_json::json!({"action": "create", "title": "x"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("create требует body"), "{err}");
    }

    #[tokio::test]
    async fn an_unknown_action_names_itself() {
        let err = call(serde_json::json!({"action": "merge"})).await.unwrap_err();
        assert!(err.to_string().contains("merge"), "{err}");
    }
}
