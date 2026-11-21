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
    /// Actions this serve may use; None = all. The users' room files and
    /// amends but does not close (D5): closing is the dev room's word.
    pub allowed_actions: Option<Vec<String>>,
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
            allowed_actions: None,
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
    pub comment: Option<String>,
    pub labels: Option<String>,
    pub path: Option<String>,
    pub sha: Option<String>,
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
        if let Some(allowed) = &self.cfg.allowed_actions {
            if !allowed.iter().any(|a| a == &action) {
                return Err(ToolError::Other(format!(
                    "err: действие '{action}' не разрешено этой комнате (можно: {})",
                    allowed.join(", ")
                )));
            }
        }
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
                self.create(title, body, a.labels.as_deref()).await
            }
            "comment" => {
                let number = a
                    .number
                    .ok_or_else(|| ToolError::Other("err: comment требует number".into()))?;
                let body = Self::need("body", &a.body, "comment")?;
                charge()?;
                self.comment(number, body).await
            }
            "close" | "reopen" => {
                let number = a.number.ok_or_else(|| {
                    ToolError::Other(format!("err: {action} требует number"))
                })?;
                // D9: a close or a reopen always says why in the issue.
                let comment = Self::need("comment", &a.comment, &action)?;
                charge()?;
                self.set_state(number, action == "close", comment).await
            }
            "edit" => {
                let number = a
                    .number
                    .ok_or_else(|| ToolError::Other("err: edit требует number".into()))?;
                let title = a.title.as_deref().map(str::trim).filter(|t| !t.is_empty());
                let body = a.body.as_deref().map(str::trim).filter(|t| !t.is_empty());
                if title.is_none() && body.is_none() {
                    return Err(ToolError::Other(
                        "err: edit требует title или body".into(),
                    ));
                }
                charge()?;
                self.edit(number, title, body).await
            }
            "commits" => self.commits().await,
            "file" => {
                let path = Self::need("path", &a.path, "file")?;
                self.read_file(path).await
            }
            "diff" => self.diff(a.sha.as_deref()).await,
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

    async fn comment(&self, number: i64, body: &str) -> Result<String, ToolError> {
        self.call(
            reqwest::Method::POST,
            &format!("/issues/{number}/comments"),
            Some(serde_json::json!({ "body": body })),
        )
        .await?;
        Ok(format!("ok: комментарий к #{number} добавлен"))
    }

    /// D9: the reason lands as a comment, then the state flips.
    async fn set_state(&self, number: i64, close: bool, comment: &str) -> Result<String, ToolError> {
        self.comment(number, comment).await?;
        self.call(
            reqwest::Method::PATCH,
            &format!("/issues/{number}"),
            Some(serde_json::json!({ "state": if close { "closed" } else { "open" } })),
        )
        .await?;
        Ok(if close {
            format!("ok: issue #{number} закрыт (причина в комментарии)")
        } else {
            format!("ok: issue #{number} переоткрыт")
        })
    }

    async fn edit(&self, number: i64, title: Option<&str>, body: Option<&str>) -> Result<String, ToolError> {
        let mut payload = serde_json::Map::new();
        if let Some(title) = title {
            payload.insert("title".into(), serde_json::json!(title));
        }
        if let Some(body) = body {
            payload.insert("body".into(), serde_json::json!(body));
        }
        self.call(
            reqwest::Method::PATCH,
            &format!("/issues/{number}"),
            Some(serde_json::Value::Object(payload)),
        )
        .await?;
        Ok(format!("ok: issue #{number} обновлён"))
    }

    async fn create(&self, title: &str, body: &str, labels: Option<&str>) -> Result<String, ToolError> {
        let mut payload = serde_json::json!({ "title": title, "body": body });
        if let Some(labels) = labels.map(str::trim).filter(|l| !l.is_empty()) {
            let list: Vec<&str> = labels.split(',').map(str::trim).filter(|l| !l.is_empty()).collect();
            payload["labels"] = serde_json::json!(list);
        }
        let value = self.call(reqwest::Method::POST, "/issues", Some(payload)).await?;
        let number = value.get("number").and_then(serde_json::Value::as_i64).unwrap_or(0);
        let url = value.get("html_url").and_then(serde_json::Value::as_str).unwrap_or("");
        Ok(format!("ok: заведён issue #{number} {url}"))
    }

    async fn commits(&self) -> Result<String, ToolError> {
        let value = self
            .call(reqwest::Method::GET, "/commits?per_page=8", None)
            .await?;
        let items = value.as_array().cloned().unwrap_or_default();
        let mut out = Vec::new();
        for item in items {
            let sha = item.get("sha").and_then(serde_json::Value::as_str).unwrap_or("");
            let msg = item
                .get("commit")
                .and_then(|c| c.get("message"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let first = msg.lines().next().unwrap_or("");
            out.push(format!("{} {}", &sha[..sha.len().min(8)], first.chars().take(90).collect::<String>()));
        }
        Ok(if out.is_empty() {
            "коммитов не видно".into()
        } else {
            out.join("\n")
        })
    }

    async fn read_file(&self, path: &str) -> Result<String, ToolError> {
        let value = self
            .call(reqwest::Method::GET, &format!("/contents/{}", path.trim_start_matches('/')), None)
            .await?;
        let content = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        // GitHub answers base64; decode without a dependency.
        let clean: String = content.chars().filter(|c| !c.is_whitespace()).collect();
        let decoded = base64_decode(&clean)?;
        let text = String::from_utf8_lossy(&decoded);
        Ok(text.chars().take(4000).collect::<String>())
    }

    async fn diff(&self, sha: Option<&str>) -> Result<String, ToolError> {
        let sha = match sha.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => s.to_string(),
            None => {
                let value = self
                    .call(reqwest::Method::GET, "/commits?per_page=1", None)
                    .await?;
                value
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|c| c.get("sha"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .to_string()
            }
        };
        if sha.is_empty() {
            return Err(ToolError::Other("err: не вижу коммитов для диффа".into()));
        }
        let url = format!("{}/repos/{}/commits/{}", self.cfg.base_url, self.cfg.repo, sha);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.cfg.token)
            .header("Accept", "application/vnd.github.diff")
            .send()
            .await
            .map_err(|e| ToolError::Other(format!("err: GitHub недоступен ({e})")))?;
        if !resp.status().is_success() {
            return Err(ToolError::Other(format!(
                "err: GitHub отказал диффу (HTTP {})",
                resp.status()
            )));
        }
        let text = resp.text().await.unwrap_or_default();
        Ok(text.chars().take(6000).collect::<String>())
    }
}

fn base64_decode(input: &str) -> Result<Vec<u8>, ToolError> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut lookup = [255u8; 256];
    for (i, b) in TABLE.iter().enumerate() {
        lookup[*b as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for b in input.bytes() {
        if b == b'=' {
            break;
        }
        let v = lookup[b as usize];
        if v == 255 {
            return Err(ToolError::Other("err: битый base64 от GitHub".into()));
        }
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Ok(out)
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
            allowed_actions: None,
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
