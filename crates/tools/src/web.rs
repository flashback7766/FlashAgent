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

fn starts_ci(hay: &str, needle: &str) -> bool {
    hay.as_bytes()
        .get(..needle.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(needle.as_bytes()))
}

/// Reduce HTML to visible text: drop script/style blocks, strip tags, decode
/// the common entities, collapse whitespace.
pub(crate) fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    while i < html.len() {
        if html.as_bytes()[i] == b'<' {
            let rest = &html[i..];
            let skip = if starts_ci(rest, "<script") {
                find_ci(rest, "</script>").map(|p| p + "</script>".len())
            } else if starts_ci(rest, "<style") {
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
    let out = decode_entities(&out);
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

/// Turn HTML entities into the characters they stand for: the named ones
/// that appear in running text, and any numbered one (`&#39;`, `&#x27;`),
/// which pages use for apostrophes and dashes in titles.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let Some(semi) = rest[..rest.len().min(12)].find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "nbsp" => Some(' '),
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "hellip" => Some('…'),
            "lsquo" => Some('‘'),
            "rsquo" => Some('’'),
            "ldquo" => Some('“'),
            "rdquo" => Some('”'),
            "middot" => Some('·'),
            "bull" => Some('•'),
            "laquo" => Some('«'),
            "raquo" => Some('»'),
            "copy" => Some('©'),
            "reg" => Some('®'),
            "trade" => Some('™'),
            "deg" => Some('°'),
            "times" => Some('×'),
            "euro" => Some('€'),
            "pound" => Some('£'),
            _ => entity.strip_prefix('#').and_then(|number| match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => number.parse().ok(),
            }).and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
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
    let approved_local_host =
        flashagent_core::url_host(url).filter(|host| flashagent_core::is_local_host(host));
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
        let same_approved_local_host = approved_local_host.as_deref() == Some(bare.as_str());
        if !same_approved_local_host && addrs.iter().any(|a| flashagent_core::is_local_ip(a.ip())) {
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

/// The class names DuckDuckGo marks a result with. The full page
/// (`html.duckduckgo.com`) and the small one (`lite.duckduckgo.com`) wrap
/// their results in different elements, quote their attributes differently
/// and shuffle the surrounding markup from time to time; the class on the
/// link and the class on the snippet are what both have kept.
const LINK_CLASSES: [&str; 2] = ["result__a", "result-link"];
const SNIPPET_CLASSES: [&str; 2] = ["result__snippet", "result-snippet"];

/// Read the results out of a DuckDuckGo results page.
///
/// Anchored on the two class names above rather than on the shape of the
/// page around them: each result is a link, and the first snippet after it
/// belongs to it.
pub fn parse_duckduckgo_html(html: &str, limit: usize) -> Vec<SearchResult> {
    let lower = html.to_lowercase();
    let mut marks: Vec<(usize, bool)> = tags_with_class(&lower, &LINK_CLASSES)
        .into_iter()
        .map(|at| (at, true))
        .chain(tags_with_class(&lower, &SNIPPET_CLASSES).into_iter().map(|at| (at, false)))
        .collect();
    marks.sort_unstable();
    marks.dedup();

    let mut results: Vec<SearchResult> = Vec::new();
    for (tag, is_link) in marks {
        if is_link {
            let Some(href) = attribute(html, tag, "href") else { continue };
            let Some(url) = result_url(&href) else { continue };
            let mut title = inner_text(html, tag);
            if title.is_empty() {
                title = "(no title)".to_string();
            }
            results.push(SearchResult { title, url, snippet: String::new() });
        } else if let Some(last) = results.last_mut() {
            if last.snippet.is_empty() {
                last.snippet = inner_text(html, tag);
            }
        }
    }
    results.truncate(limit);
    results
}

/// Where every tag carrying one of `classes` starts. The class has to be a
/// whole name in the attribute, not the beginning of a longer one.
fn tags_with_class(lower: &str, classes: &[&str; 2]) -> Vec<usize> {
    let mut out = Vec::new();
    for class in classes {
        let mut from = 0;
        while let Some(found) = lower[from..].find(class) {
            let at = from + found;
            from = at + class.len();
            let part_of_a_longer_name = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
            if lower[..at].ends_with(part_of_a_longer_name) || lower[from..].starts_with(part_of_a_longer_name) {
                continue;
            }
            if let Some(tag) = lower[..at].rfind('<') {
                out.push(tag);
            }
        }
    }
    out
}

/// The text of the `<...>` starting at `tag`.
fn tag_text(html: &str, tag: usize) -> Option<&str> {
    let rest = html.get(tag..)?;
    let end = rest.find('>')?;
    Some(&rest[..end])
}

/// The value of an attribute of the tag starting at `tag`, in either kind of
/// quotes. Entities are left as written; the callers decode what they need.
fn attribute(html: &str, tag: usize, name: &str) -> Option<String> {
    let text = tag_text(html, tag)?;
    let lower = text.to_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let before_is_space = lower[..at].ends_with(char::is_whitespace);
        let rest = lower[from..].trim_start();
        if !before_is_space || !rest.starts_with('=') {
            continue;
        }
        let value = text[from..].trim_start().strip_prefix('=')?.trim_start();
        let quote = value.chars().next()?;
        return match quote {
            '"' | '\'' => value[1..].split(quote).next().map(str::to_string),
            _ => value.split_whitespace().next().map(str::to_string),
        };
    }
    None
}

/// The visible text of the element starting at `tag`, up to its closing tag.
fn inner_text(html: &str, tag: usize) -> String {
    let Some(text) = tag_text(html, tag) else { return String::new() };
    let name: String = text
        .trim_start_matches('<')
        .chars()
        .take_while(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    let body_at = tag + text.len() + 1;
    let Some(body) = html.get(body_at..) else { return String::new() };
    let end = find_ci(body, &format!("</{name}")).unwrap_or(body.len());
    html_to_text(&body[..end])
}

/// The page a result link leads to. DuckDuckGo wraps it in a redirect of its
/// own (`/l/?uddg=...`); its ads and its own pages are not results.
fn result_url(href: &str) -> Option<String> {
    let href = href.replace("&amp;", "&");
    if let Some(target) = href.split("uddg=").nth(1) {
        let encoded = target.split('&').next().unwrap_or(target);
        let url = urlencoding_decode(encoded);
        return url.starts_with("http").then_some(url);
    }
    let bare = href.trim_start_matches("https:").trim_start_matches("http:");
    if bare.starts_with("//duckduckgo.com") || bare.starts_with("//ad.") || bare.starts_with('/') {
        return None;
    }
    href.starts_with("http").then_some(href)
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

/// The pages DuckDuckGo serves without an API key, in the order they are
/// tried: the full results page first, the small one as a fallback for when
/// the first answers with something that is not a results page at all.
const FREE_ENDPOINTS: [&str; 2] =
    ["https://html.duckduckgo.com/html/", "https://lite.duckduckgo.com/lite/"];

/// Free web search via DuckDuckGo, no API key.
///
/// A query that genuinely matches nothing says so. A page that is not a
/// results page — a rate-limit notice, a bot check, a redesign the parser
/// does not know — is an error, not an empty answer, because the model can
/// do something about the first and nothing about the second.
pub async fn search_free(http: &reqwest::Client, query: &str, count: u32) -> Result<String, ToolError> {
    let mut trouble = String::new();
    for endpoint in FREE_ENDPOINTS {
        let body = match fetch_results_page(http, endpoint, query).await {
            Ok(body) => body,
            Err(e) => {
                trouble = e.to_string();
                continue;
            }
        };
        let results = parse_duckduckgo_html(&body, count as usize);
        if !results.is_empty() {
            let mut out = String::new();
            for (i, r) in results.iter().enumerate() {
                out.push_str(&format!("{}. {}\n{}\n{}\n\n", i + 1, r.title, r.url, r.snippet));
            }
            return Ok(out);
        }
        if says_there_is_nothing(&body) {
            return Ok("no results".into());
        }
        trouble = describe_unusable_page(&body);
    }
    Err(ToolError::Other(format!(
        "search: DuckDuckGo did not return results ({trouble}). Trying again in a moment usually works; \
         set BRAVE_API_KEY in the environment to search with a key instead."
    )))
}

async fn fetch_results_page(http: &reqwest::Client, endpoint: &str, query: &str) -> Result<String, ToolError> {
    let resp = http
        .get(endpoint)
        .query(&[("q", query)])
        .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0")
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await
        .map_err(|e| ToolError::Other(format!("search: {e}")))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| ToolError::Other(format!("read search body: {e}")))?;
    if !status.is_success() {
        return Err(ToolError::Other(format!("HTTP {status}: {}", truncate_body(&body, 500))));
    }
    Ok(body)
}

/// Whether the page is a results page reporting that it found nothing.
fn says_there_is_nothing(body: &str) -> bool {
    let text = html_to_text(body).to_lowercase();
    text.contains("no results") || text.contains("not match any documents")
}

/// What to say about a page with no results on it that does not claim to
/// have none: usually a bot check, otherwise a page shaped differently than
/// the parser expects.
fn describe_unusable_page(body: &str) -> String {
    let text = html_to_text(body).to_lowercase();
    if text.contains("anomaly") || text.contains("unusual traffic") || text.contains("are you a robot") {
        "it asked for a bot check instead".to_string()
    } else {
        format!("the page it sent holds no results ({} characters)", body.len())
    }
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

    #[tokio::test]
    async fn approval_for_one_local_host_does_not_cover_a_redirect_to_another() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            let _ = sock.read(&mut buf).await;
            sock.write_all(
                b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.2:9/private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        });

        let err = fetch_text(&format!("http://127.0.0.1:{port}/start")).await.unwrap_err().to_string();
        assert!(err.contains("local address"), "got: {err}");
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
    fn html_to_text_ignores_script_and_style_tags_case_insensitively() {
        let html = "<STYLE>.secret { display: block }</STYLE><p>Visible</p><SCRIPT>alert('secret')</SCRIPT>";
        let text = html_to_text(html);
        assert_eq!(text, "Visible");
    }

    #[test]
    fn truncate_body_is_char_safe() {
        let s = "✨hello".repeat(100);
        assert!(truncate_body(&s, 7).is_char_boundary(0));
    }

    /// One result, written the way `html.duckduckgo.com` writes it: the
    /// class names are not alone in their attributes, and the link is
    /// DuckDuckGo's own redirect.
    const FULL_PAGE: &str = r##"
    <div class="result results_links results_links_deep web-result ">
      <div class="links_main links_deep result__body">
        <h2 class="result__title">
          <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&amp;rut=13d8">Rust Programming Language</a>
        </h2>
        <div class="result__extras"><a class="result__url" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&amp;rut=13d8">rust-lang.org</a></div>
        <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&amp;rut=13d8"><b>Rust</b> is fast and it&#x27;s reliable.</a>
      </div>
    </div>
    "##;

    /// The same result from `lite.duckduckgo.com`: a table, single-quoted
    /// attributes, the snippet in a later row.
    const LITE_PAGE: &str = r##"
    <table>
      <tr><td class='result-snippet'>Not this one, it belongs to nothing.</td></tr>
      <tr><td>
        <a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust%2Dlang.org%2F&amp;rut=13d8" class='result-link'>Rust Programming Language</a>
      </td></tr>
      <tr><td class='result-snippet'><b>Rust</b> is fast and it&#x27;s reliable.</td></tr>
    </table>
    "##;

    #[test]
    fn a_result_is_read_from_the_page_duckduckgo_actually_serves() {
        for (name, page) in [("full", FULL_PAGE), ("lite", LITE_PAGE)] {
            let results = parse_duckduckgo_html(page, 5);
            assert_eq!(results.len(), 1, "{name}: {results:?}");
            assert_eq!(results[0].title, "Rust Programming Language", "{name}");
            // Unwrapped from DuckDuckGo's redirect, and the escapes undone.
            assert_eq!(results[0].url, "https://rust-lang.org/", "{name}");
            assert_eq!(results[0].snippet, "Rust is fast and it's reliable.", "{name}");
        }
    }

    #[test]
    fn a_snippet_belongs_to_the_link_above_it() {
        let two = format!("{FULL_PAGE}{FULL_PAGE}");
        let results = parse_duckduckgo_html(&two, 5);
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.snippet == "Rust is fast and it's reliable."), "{results:?}");
        assert_eq!(parse_duckduckgo_html(&two, 1).len(), 1, "the limit is kept");
    }

    #[test]
    fn duckduckgos_own_links_and_ads_are_not_results() {
        let page = r##"
        <a class="result__a" href="//duckduckgo.com/y.js?ad_provider=bing">An advert</a>
        <a class="result__a" href="/settings">Settings</a>
        <a class="result__a-different" href="https://example.com/">Not a result link at all</a>
        "##;
        assert_eq!(parse_duckduckgo_html(page, 5), Vec::new());
    }

    #[test]
    fn a_page_without_results_is_told_apart_from_one_that_found_nothing() {
        assert!(says_there_is_nothing("<div>No results.</div>"));
        assert!(!says_there_is_nothing("<div>Our servers are busy</div>"));
        assert!(describe_unusable_page("<p>If this error persists, please let us know: this error was reported as an anomaly.</p>")
            .contains("bot check"));
        assert!(describe_unusable_page("<p>Something new</p>").contains("no results"));
    }

    #[test]
    fn entities_become_the_characters_they_stand_for() {
        assert_eq!(html_to_text("<p>a &amp; b &#39;c&#39; &#x27;d&#x27; &mdash; e&nbsp;f</p>"), "a & b 'c' 'd' — e f");
        // Something that is not an entity is left alone.
        assert_eq!(html_to_text("<p>Fish &amp chips &#zz; R&D</p>"), "Fish &amp chips &#zz; R&D");
    }
}
