//! Network tools: `web_search` (DuckDuckGo by default, or a configured SearXNG instance) and
//! `web_fetch` (URL → readable text).
//!
//! Both are classified as read-only by the provider loops, so they execute inside
//! `tokio::task::spawn_blocking`. We drive the async `reqwest` client from there using a small
//! current-thread runtime, which keeps these helpers synchronous like the other tools.

use std::time::Duration;

use serde_json::Value;

use super::paths::err;
use super::MAX_TOOL_OUTPUT_CHARS;

const DEFAULT_SEARCH_COUNT: usize = 8;
const MAX_SEARCH_COUNT: usize = 20;
const DEFAULT_FETCH_CHARS: usize = 20_000;
const HTTP_TIMEOUT_SECS: u64 = 20;

const DEFAULT_USER_AGENT: &str = "oxi/0.4";
/// Zero-config search backend used when no SearXNG URL is set. DuckDuckGo's HTML endpoint
/// needs no API key, but it serves an anomaly-challenge page (HTTP 202) to clients that
/// don't look like a browser, so requests to it carry a full browser header set.
const DDG_HTML_URL: &str = "https://html.duckduckgo.com/html/";
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const BROWSER_HEADERS: &[(&str, &str)] = &[
    (
        "Accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    ),
    ("Accept-Language", "en-US,en;q=0.9"),
    ("Referer", "https://html.duckduckgo.com/"),
];

/// Response from an HTTP GET: status, content-type header, and decoded body.
struct HttpResponse {
    status: u16,
    content_type: String,
    body: String,
}

/// Perform a blocking HTTP GET. `accept_invalid_certs` is enabled for the user's local SearXNG
/// instance (typically a self-signed cert on an mDNS host); it stays off for arbitrary fetches.
fn http_get(
    url: &str,
    query: &[(&str, &str)],
    accept_invalid_certs: bool,
    user_agent: &str,
    headers: &[(&str, &str)],
) -> Result<HttpResponse, String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .user_agent(user_agent)
            .danger_accept_invalid_certs(accept_invalid_certs)
            .build()
            .map_err(|e| format!("client: {e}"))?;
        let mut req = client.get(url).query(query);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status().as_u16();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = resp.text().await.map_err(|e| format!("read body: {e}"))?;
        Ok(HttpResponse {
            status,
            content_type,
            body,
        })
    })
}

pub(crate) fn tool_web_search(base_url: &str, args: &Value) -> Result<String, String> {
    let query = args
        .get("query")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("missing query"))?;
    let count = args
        .get("count")
        .and_then(|x| x.as_u64())
        .map(|n| (n as usize).clamp(1, MAX_SEARCH_COUNT))
        .unwrap_or(DEFAULT_SEARCH_COUNT);

    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        // No SearXNG configured: fall back to the built-in zero-config DuckDuckGo backend.
        return ddg_search(query, count);
    }
    let url = format!("{base}/search");
    let resp = http_get(
        &url,
        &[("q", query), ("format", "json")],
        true,
        DEFAULT_USER_AGENT,
        &[],
    )?;
    if resp.status == 403 {
        return Err(err(
            "SearXNG returned 403 — enable the JSON output format in its settings.yml (search.formats: [html, json])",
        ));
    }
    if resp.status >= 400 {
        return Err(format!("SearXNG returned HTTP {}", resp.status));
    }

    let json: Value =
        serde_json::from_str(&resp.body).map_err(|e| format!("invalid JSON from SearXNG: {e}"))?;
    let results = json
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        return Ok(format!("No results for: {query}"));
    }

    let mut out = format!("Search results for: {query}\n");
    for item in results.iter().take(count) {
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("(untitled)");
        let link = item.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let snippet = item
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        out.push_str(&format!("\n- {title}\n  {link}\n"));
        if !snippet.is_empty() {
            out.push_str(&format!("  {snippet}\n"));
        }
    }
    Ok(truncate(out, MAX_TOOL_OUTPUT_CHARS))
}

/// Search DuckDuckGo's HTML endpoint and format the results like the SearXNG path.
fn ddg_search(query: &str, count: usize) -> Result<String, String> {
    let resp = http_get(
        DDG_HTML_URL,
        &[("q", query)],
        false,
        BROWSER_USER_AGENT,
        BROWSER_HEADERS,
    )?;
    if resp.status >= 400 {
        return Err(format!("DuckDuckGo returned HTTP {}", resp.status));
    }

    let results = parse_ddg_results(&resp.body, count);
    if results.is_empty() {
        if resp.body.contains("anomaly") || resp.body.contains("challenge") {
            return Err(
                "DuckDuckGo rate-limited this request — retry shortly, or configure a SearXNG URL in Settings"
                    .to_string(),
            );
        }
        return Ok(format!("No results for: {query}"));
    }

    let mut out = format!("Search results for: {query}\n");
    for (title, link, snippet) in results {
        out.push_str(&format!("\n- {title}\n  {link}\n"));
        if !snippet.is_empty() {
            out.push_str(&format!("  {snippet}\n"));
        }
    }
    Ok(truncate(out, MAX_TOOL_OUTPUT_CHARS))
}

/// Extract `(title, url, snippet)` triples from a DuckDuckGo HTML results page. String-based
/// like [`html_to_text`]: each organic result carries a `result__a` anchor (title + redirect
/// href) followed by a `result__snippet` element.
fn parse_ddg_results(html: &str, count: usize) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while out.len() < count {
        let Some(rel) = html[cursor..].find("class=\"result__a\"") else {
            break;
        };
        let anchor = cursor + rel;
        // The next result__a marker bounds this result's snippet search.
        let next = html[anchor + 1..]
            .find("class=\"result__a\"")
            .map(|r| anchor + 1 + r)
            .unwrap_or(html.len());
        cursor = next;

        let Some(href) = attr_after(html, anchor, "href=\"") else {
            continue;
        };
        let link = resolve_ddg_href(&decode_entities(&href));
        // Ads route through DuckDuckGo's click tracker instead of a plain redirect.
        if link.is_empty() || link.contains("duckduckgo.com/y.js") {
            continue;
        }
        let Some(title_html) = element_text(html, anchor, next) else {
            continue;
        };
        let title = clean_fragment(&title_html);
        if title.is_empty() {
            continue;
        }

        let snippet = html[anchor..next]
            .find("class=\"result__snippet\"")
            .and_then(|r| element_text(html, anchor + r, next))
            .map(|s| clean_fragment(&s))
            .unwrap_or_default();
        out.push((title, link, snippet));
    }
    out
}

/// Value of the first `prefix`-delimited attribute at or after `from` (e.g. `href="…"`).
fn attr_after(html: &str, from: usize, prefix: &str) -> Option<String> {
    let start = from + html[from..].find(prefix)? + prefix.len();
    let end = start + html[start..].find('"')?;
    Some(html[start..end].to_string())
}

/// Inner HTML of the element whose opening tag contains position `pos`, bounded by `limit`.
/// Runs to the element's own closing tag so inline markup like `<b>` stays part of the text
/// (assumes no nesting of the same tag, which holds for DDG's result anchors).
fn element_text(html: &str, pos: usize, limit: usize) -> Option<String> {
    let tag_start = html[..pos].rfind('<')?;
    let name: String = html[tag_start + 1..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    let open_end = pos + html[pos..limit].find('>')? + 1;
    let close = open_end + html[open_end..limit].find(&format!("</{name}"))?;
    Some(html[open_end..close].to_string())
}

/// Strip inline tags (DDG bolds query terms with `<b>`), decode entities, collapse whitespace.
fn clean_fragment(fragment: &str) -> String {
    let mut text = String::with_capacity(fragment.len());
    let mut in_tag = false;
    for ch in fragment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    decode_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// DuckDuckGo wraps result links in a `//duckduckgo.com/l/?uddg=<encoded>` redirect;
/// unwrap it back to the destination URL. Direct links pass through unchanged.
fn resolve_ddg_href(href: &str) -> String {
    let absolute = if let Some(rest) = href.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        href.to_string()
    };
    if absolute.contains("duckduckgo.com/l/") {
        if let Ok(parsed) = url::Url::parse(&absolute) {
            if let Some((_, dest)) = parsed.query_pairs().find(|(k, _)| k == "uddg") {
                return dest.into_owned();
            }
        }
    }
    absolute
}

pub(crate) fn tool_web_fetch(args: &Value) -> Result<String, String> {
    let url = args
        .get("url")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("missing url"))?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(err("url must start with http:// or https://"));
    }
    let max_chars = args
        .get("max_chars")
        .and_then(|x| x.as_u64())
        .map(|n| (n as usize).min(MAX_TOOL_OUTPUT_CHARS))
        .unwrap_or(DEFAULT_FETCH_CHARS);

    let resp = http_get(url, &[], false, DEFAULT_USER_AGENT, &[])?;
    if resp.status >= 400 {
        return Err(format!("HTTP {} fetching {url}", resp.status));
    }

    let text = if resp.content_type.contains("html") || looks_like_html(&resp.body) {
        html_to_text(&resp.body)
    } else {
        resp.body
    };
    Ok(truncate(text, max_chars))
}

fn truncate(mut s: String, max: usize) -> String {
    if s.chars().count() > max {
        let end = s
            .char_indices()
            .nth(max)
            .map(|(i, _)| i)
            .unwrap_or_else(|| s.len());
        s.truncate(end);
        s.push_str("\n…[truncated]");
    }
    s
}

fn looks_like_html(body: &str) -> bool {
    let head = body.trim_start();
    let lower = head[..head.len().min(256)].to_ascii_lowercase();
    lower.starts_with("<!doctype html") || lower.starts_with("<html") || lower.contains("<body")
}

/// Strip HTML down to readable plain text. Intentionally lightweight (no DOM parsing): drops
/// `<script>`/`<style>` blocks, turns common block tags into line breaks, removes the remaining
/// tags, decodes a handful of entities, and collapses excess whitespace.
fn html_to_text(html: &str) -> String {
    let no_scripts = strip_blocks(html, "script");
    let no_styles = strip_blocks(&no_scripts, "style");

    let mut text = String::with_capacity(no_styles.len());
    let s = no_styles.as_str();
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with('<') {
            // Find the end of the tag; if there is no closing '>', drop the rest.
            match rest.find('>') {
                Some(gt) => {
                    let tag = rest[..gt].to_ascii_lowercase();
                    if is_block_tag(&tag) {
                        text.push('\n');
                    }
                    i += gt + 1;
                }
                None => break,
            }
        } else {
            // Advance one full UTF-8 char so multi-byte text is preserved intact.
            let ch = rest.chars().next().unwrap();
            text.push(ch);
            i += ch.len_utf8();
        }
    }

    let decoded = decode_entities(&text);
    collapse_whitespace(&decoded)
}

fn strip_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut cursor = 0;
    while let Some(rel) = lower[cursor..].find(&open) {
        let start = cursor + rel;
        out.push_str(&html[cursor..start]);
        match lower[start..].find(&close) {
            Some(crel) => cursor = start + crel + close.len(),
            None => {
                cursor = html.len();
                break;
            }
        }
    }
    out.push_str(&html[cursor..]);
    out
}

fn is_block_tag(tag: &str) -> bool {
    const BLOCK: &[&str] = &[
        "<br",
        "<p",
        "</p",
        "<div",
        "</div",
        "<li",
        "</li",
        "<ul",
        "</ul",
        "<ol",
        "</ol",
        "<tr",
        "</tr",
        "<h1",
        "</h1",
        "<h2",
        "</h2",
        "<h3",
        "</h3",
        "<h4",
        "</h4",
        "<h5",
        "</h5",
        "<h6",
        "</h6",
        "<section",
        "</section",
        "<article",
        "</article",
        "<header",
        "</header",
        "<footer",
    ];
    BLOCK.iter().any(|b| tag.starts_with(b))
}

fn decode_entities(s: &str) -> String {
    s.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
}

fn collapse_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0;
    for line in s.lines() {
        // Collapse runs of spaces/tabs inside the line.
        let trimmed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(&trimmed);
            out.push('\n');
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_to_text_strips_tags_and_scripts() {
        let html = "<html><head><style>.a{color:red}</style></head><body><h1>Title</h1><script>alert(1)</script><p>Hello &amp; welcome</p><p>Second</p></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Title"));
        assert!(text.contains("Hello & welcome"));
        assert!(text.contains("Second"));
        assert!(!text.contains("alert"));
        assert!(!text.contains("color:red"));
        assert!(!text.contains('<'));
    }

    #[test]
    fn html_to_text_preserves_non_ascii() {
        let text = html_to_text("<p>Salut, în această zi însorită!</p>");
        assert!(text.contains("în această zi însorită"));
    }

    #[test]
    fn truncate_appends_marker() {
        let s = "a".repeat(100);
        let out = truncate(s, 10);
        assert!(out.starts_with("aaaaaaaaaa"));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn looks_like_html_detects_doctype() {
        assert!(looks_like_html("  <!DOCTYPE html><html>"));
        assert!(!looks_like_html("{\"json\": true}"));
    }

    #[test]
    fn web_fetch_rejects_non_http() {
        let res = tool_web_fetch(&serde_json::json!({"url": "file:///etc/passwd"}));
        assert!(res.is_err());
    }

    #[test]
    fn web_search_requires_query() {
        let res = tool_web_search("https://example.invalid", &serde_json::json!({}));
        assert!(res.is_err());
    }

    #[test]
    fn parse_ddg_results_extracts_title_link_snippet() {
        let html = concat!(
            "<div class=\"result\">",
            "<a rel=\"nofollow\" class=\"result__a\" ",
            "href=\"//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2F&amp;rut=abc\">",
            "The <b>Rust</b> Language</a>",
            "<a class=\"result__snippet\" href=\"#\">A language empowering everyone &amp; more.</a>",
            "</div>",
            "<div class=\"result\">",
            "<a class=\"result__a\" href=\"https://example.com/direct\">Direct link</a>",
            "</div>",
        );
        let results = parse_ddg_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "The Rust Language");
        assert_eq!(results[0].1, "https://www.rust-lang.org/");
        assert_eq!(results[0].2, "A language empowering everyone & more.");
        assert_eq!(results[1].1, "https://example.com/direct");
        assert!(results[1].2.is_empty());
    }

    #[test]
    fn parse_ddg_results_skips_ads_and_respects_count() {
        let html = concat!(
            "<a class=\"result__a\" href=\"https://duckduckgo.com/y.js?ad_domain=x\">Ad</a>",
            "<a class=\"result__a\" href=\"https://one.example\">One</a>",
            "<a class=\"result__a\" href=\"https://two.example\">Two</a>",
        );
        let results = parse_ddg_results(html, 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, "https://one.example");
    }

    /// Live network test: `cargo test ddg_search_live -- --ignored --nocapture`.
    #[test]
    #[ignore = "hits the real DuckDuckGo endpoint"]
    fn ddg_search_live() {
        let out = ddg_search("rust programming language", 5).expect("ddg_search failed");
        println!("{out}");
        assert!(out.contains("Search results for:"));
        assert!(out.contains("https://"));
    }

    #[test]
    fn resolve_ddg_href_unwraps_redirect() {
        let link = resolve_ddg_href("//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fserde&rut=x");
        assert_eq!(link, "https://docs.rs/serde");
        assert_eq!(
            resolve_ddg_href("https://example.com/page"),
            "https://example.com/page"
        );
    }
}
