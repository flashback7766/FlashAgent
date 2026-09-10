//! Web tools: fetch a URL as plain text and search via the Brave Search API.

use serde_json::Value;

use crate::ToolError;

fn truncate_body(body: &str, max: usize) -> &str {
    if body.len() <= max {
        body
    } else {
        let mut end = max;
        while !body.is_char_boundary(end) {
            end += 1;
        }
        &body[..end]
    }
}

/// Case-insensitive byte search for an ASCII needle.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let hay = hay.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || n.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - n.len()).find(|&i| hay[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Reduce HTML to visible text: drop script/style blocks, strip tags, decode
/// the common entities, collapse whitespace.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    while i < html.len() {
        if html.as_bytes()[i] == b'<' {
            let rest = &html[i..];
            let skip = if rest.as_bytes().starts_with(b"<script") {
                find_ci(rest, "</script>").map(|p| p + "</script>".len())
            } else if rest.as_bytes().starts_with(b"<style") {
                find_ci(rest, "</style>").map(|p| p + "</style>".len())
            } else {
                rest.find('>').map(|p| p + 1)
            };
            match skip {
                Some(end) => {
                    i += end;
                    out.push(' ');
                }
                None => break,
            }
        } else {
            let next = html[i..].find('<').map_or(html.len(), |p| i + p);
            out.push_str(&html[i..next]);
            i = next;
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_ws = false;
    for ch in out.chars() {
        let ws = ch.is_whitespace();
        if !(ws && prev_ws) {
            collapsed.push(if ws { ' ' } else { ch });
        }
        prev_ws = ws;
    }
    collapsed.trim().to_string()
}

/// Fetch a URL; HTML responses are reduced to plain text.
pub async fn fetch_text(http: &reqwest::Client, url: &str) -> Result<String, ToolError> {
    let resp = http
        .get(url)
        .send()
        .await
        .map_err(|e| ToolError::Other(format!("fetch: {e}")))?;
    let status = resp.status();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = resp
        .text()
        .await
        .map_err(|e| ToolError::Other(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(ToolError::Other(format!("HTTP {status}: {}", truncate_body(&body, 500))));
    }
    let is_html = content_type.contains("html")
        || (content_type.is_empty() && body.trim_start().starts_with('<'));
    Ok(if is_html { html_to_text(&body) } else { body })
}

/// Search the web via the Brave Search API.
pub async fn search(
    http: &reqwest::Client,
    api_key: &str,
    query: &str,
    count: u32,
) -> Result<String, ToolError> {
    let count_str = count.to_string();
    let resp = http
        .get("https://api.search.brave.com/res/v1/web/search")
        .header("X-Subscription-Token", api_key)
        .query(&[("q", query), ("count", count_str.as_str())])
        .send()
        .await
        .map_err(|e| ToolError::Other(format!("search: {e}")))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| ToolError::Other(format!("read body: {e}")))?;
    if !status.is_success() {
        return Err(ToolError::Other(format!("HTTP {status}: {}", truncate_body(&body, 500))));
    }
    let json: Value = serde_json::from_str(&body)
        .map_err(|e| ToolError::Other(format!("bad search response: {e}")))?;
    let Some(results) = json.pointer("/web/results").and_then(Value::as_array) else {
        return Ok("no results".into());
    };
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        let title = r.get("title").and_then(Value::as_str).unwrap_or("(no title)");
        let url = r.get("url").and_then(Value::as_str).unwrap_or("");
        let desc = r.get("description").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("{}. {title}\n{url}\n{desc}\n\n", i + 1));
    }
    Ok(out)
}

/// Search result item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// Parse HTML from DuckDuckGo search.
pub fn parse_duckduckgo_html(html: &str, limit: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let lower = html.to_lowercase();
    let mut cursor = 0;

    while let Some(rel_start) = lower[cursor..].find("class=\"result__body\"")
        .or_else(|| lower[cursor..].find("class=\"result-link\""))
        .or_else(|| lower[cursor..].find("<div class=\"result "))
    {
        let abs_start = cursor + rel_start;
        let block_end = lower[abs_start..].find("</div>\n</div>")
            .or_else(|| lower[abs_start..].find("</td></tr>"))
            .or_else(|| lower[abs_start..].find("<div class=\"result "))
            .map(|e| abs_start + e)
            .unwrap_or_else(|| (abs_start + 1500).min(html.len()));

        let block = &html[abs_start..block_end];

        let mut title = String::new();
        let mut url = String::new();
        let mut snippet = String::new();

        if let Some(href_pos) = block.find("href=\"") {
            let rest = &block[href_pos + 6..];
            if let Some(quote_end) = rest.find('"') {
                let raw_url = &rest[..quote_end];
                if let Some(uddg) = raw_url.split("uddg=").nth(1) {
                    let clean = uddg.split('&').next().unwrap_or(uddg);
                    url = urlencoding_decode(clean);
                } else if !raw_url.starts_with("//duckduckgo") {
                    url = raw_url.to_string();
                }
                if let Some(tag_end) = rest[quote_end..].find('>') {
                    let after_tag = &rest[quote_end + tag_end + 1..];
                    if let Some(a_close) = after_tag.find("</a>") {
                        title = html_to_text(&after_tag[..a_close]);
                    }
                }
            }
        }

        if let Some(snip_pos) = block.find("class=\"result__snippet\"")
            .or_else(|| block.find("class=\"result-snippet\""))
        {
            let rest = &block[snip_pos..];
            if let Some(tag_end) = rest.find('>') {
                let after_tag = &rest[tag_end + 1..];
                if let Some(tag_close) = after_tag.find("</") {
                    snippet = html_to_text(&after_tag[..tag_close]);
                }
            }
        }

        if !title.is_empty() || !url.is_empty() {
            if title.is_empty() {
                title = "(no title)".to_string();
            }
            results.push(SearchResult { title, url, snippet });
            if results.len() >= limit {
                break;
            }
        }

        cursor = block_end.max(abs_start + 20);
        if cursor >= html.len() {
            break;
        }
    }

    results
}

fn urlencoding_decode(input: &str) -> String {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Free web search via DuckDuckGo (zero API keys, 100% free).
pub async fn search_free(
    http: &reqwest::Client,
    query: &str,
    count: u32,
) -> Result<String, ToolError> {
    let resp = http
        .get("https://html.duckduckgo.com/html/")
        .query(&[("q", query)])
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0")
        .send()
        .await
        .map_err(|e| ToolError::Other(format!("search: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| ToolError::Other(format!("read search body: {e}")))?;
    if !status.is_success() {
        return Err(ToolError::Other(format!("HTTP {status}: {}", truncate_body(&body, 500))));
    }
    let results = parse_duckduckgo_html(&body, count as usize);
    if results.is_empty() {
        return Ok("no results".into());
    }
    let mut out = String::new();
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!("{}. {}\n{}\n{}\n\n", i + 1, r.title, r.url, r.snippet));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_extracts_visible_text() {
        let html = r#"<html><head><style>.x{color:red}</style></head><body><h1>Heading</h1><p>Hello &amp; goodbye</p><script>evil()</script></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("Heading"), "got: {text}");
        assert!(text.contains("Hello & goodbye"));
        assert!(!text.contains("evil"));
        assert!(!text.contains("color:red"));
    }

    #[test]
    fn truncate_body_is_char_safe() {
        let s = "✨hello".repeat(100);
        assert!(truncate_body(&s, 7).is_char_boundary(0));
    }

    #[test]
    fn parse_duckduckgo_html_extracts_results() {
        let sample = r#"
        <div class="result result--default">
            <div class="result__body">
                <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Ftest">Example Title</a>
                <a class="result__snippet">This is an example snippet.</a>
            </div>
        </div>
        "#;
        let results = parse_duckduckgo_html(sample, 5);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "Example Title");
        assert_eq!(results[0].url, "https://example.com/test");
        assert_eq!(results[0].snippet, "This is an example snippet.");
    }
}
