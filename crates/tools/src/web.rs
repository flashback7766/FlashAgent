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

/// Most redirects one fetch follows.
const MAX_REDIRECTS: usize = 5;

/// Fetch a URL; HTML responses are reduced to plain text.
///
/// A local address — this machine, the local network, a cloud metadata
/// endpoint — is fetched only when the URL names it outright, because only
/// then has the permission layer recognised it and asked the user. One that
/// turns out local some other way (a DNS name pointing at 127.0.0.1, an IP
/// spelled `127.1`, a redirect from a public page) is refused. The address
/// that was checked is the one connected to, so a name cannot resolve to a
/// public address for the check and a local one for the request, and every
/// redirect is checked the same way before it is followed.
pub async fn fetch_text(url: &str) -> Result<String, ToolError> {
    let approved_local = flashagent_core::url_host(url).is_some_and(|h| flashagent_core::is_local_host(&h));
    let mut current =
        reqwest::Url::parse(url).map_err(|e| ToolError::Other(format!("fetch: bad URL {url}: {e}")))?;
    for _ in 0..=MAX_REDIRECTS {
        if !matches!(current.scheme(), "http" | "https") {
            return Err(ToolError::Other(format!("fetch: only http and https URLs can be fetched, not {}", current.scheme())));
        }
        let host = current.host_str().ok_or_else(|| ToolError::Other(format!("fetch: {current} has no host")))?.to_string();
        let bare = host.trim_start_matches('[').trim_end_matches(']').to_string();
        let port = current.port_or_known_default().unwrap_or(80);
        let literal = bare.parse::<std::net::IpAddr>().ok();
        let addrs: Vec<std::net::SocketAddr> = match literal {
            Some(ip) => vec![std::net::SocketAddr::new(ip, port)],
            None => tokio::net::lookup_host((bare.as_str(), port))
                .await
                .map_err(|e| ToolError::Other(format!("fetch: cannot resolve {bare}: {e}")))?
                .collect(),
        };
        let Some(first) = addrs.first().copied() else {
            return Err(ToolError::Other(format!("fetch: {bare} has no address")));
        };
        if !approved_local && addrs.iter().any(|a| flashagent_core::is_local_ip(a.ip())) {
            return Err(ToolError::Other(format!(
                "fetch: {host} leads to a local address ({}); a local address is fetched only when the URL names it, so it can be approved",
                first.ip()
            )));
        }
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none());
        if literal.is_none() {
            builder = builder.resolve(&bare, first);
        }
        let client = builder.build().map_err(|e| ToolError::Other(format!("fetch: {e}")))?;
        let resp = client.get(current.clone()).send().await.map_err(|e| ToolError::Other(format!("fetch: {e}")))?;
        if resp.status().is_redirection() {
            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ToolError::Other(format!("fetch: HTTP {} without a location", resp.status())))?;
            current = current.join(location).map_err(|e| ToolError::Other(format!("fetch: bad redirect {location}: {e}")))?;
            continue;
        }
        return read_fetched(resp).await;
    }
    Err(ToolError::Other(format!("fetch: more than {MAX_REDIRECTS} redirects")))
}

async fn read_fetched(resp: reqwest::Response) -> Result<String, ToolError> {
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

    #[tokio::test]
    async fn only_http_is_fetched() {
        let err = fetch_text("file:///etc/passwd").await.unwrap_err().to_string();
        assert!(err.contains("only http and https"), "{err}");
    }

    #[tokio::test]
    async fn a_local_address_the_url_does_not_name_outright_is_refused() {
        // Both are 127.0.0.1 once parsed, but neither looks local as written,
        // so the permission layer never asked about them.
        for url in ["http://127.1:9/", "http://0x7f.0.0.1:9/"] {
            let err = fetch_text(url).await.unwrap_err().to_string();
            assert!(err.contains("local address"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn a_local_address_named_outright_is_left_to_the_permission_layer() {
        // Port 9 has nothing listening: the error is the connection's, not
        // the guard's.
        let err = fetch_text("http://127.0.0.1:9/").await.unwrap_err().to_string();
        assert!(!err.contains("leads to a local address"), "{err}");
    }

    #[tokio::test]
    async fn redirects_are_followed_one_checked_hop_at_a_time() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            for _ in 0..2 {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap();
                let request = String::from_utf8_lossy(&buf[..n]).to_string();
                let reply = if request.starts_with("GET /start") {
                    "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello".to_string()
                };
                sock.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        let body = fetch_text(&format!("http://127.0.0.1:{port}/start")).await.unwrap();
        assert_eq!(body, "hello");
    }

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
