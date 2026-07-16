//! Network tools: `web_search` (DuckDuckGo by default, or a configured SearXNG instance) and
//! `web_fetch` (URL → readable text).
//!
//! Both are classified as read-only by the provider loops, so they execute inside
//! `tokio::task::spawn_blocking`. We drive the async `reqwest` client from there using a small
//! current-thread runtime, which keeps these helpers synchronous like the other tools.

use std::net::{IpAddr, ToSocketAddrs};
use std::time::Duration;

use futures_util::StreamExt;
use serde_json::Value;

use super::MAX_TOOL_OUTPUT_CHARS;
use super::paths::err;

const DEFAULT_SEARCH_COUNT: usize = 8;
const MAX_SEARCH_COUNT: usize = 20;
const DEFAULT_FETCH_CHARS: usize = 20_000;
const HTTP_TIMEOUT_SECS: u64 = 20;
/// Maximum decompressed response body retained for `web_fetch`. The final tool text is much
/// smaller, but HTML needs some headroom before tags/navigation are stripped.
const MAX_FETCH_BODY_BYTES: usize = 2 * 1024 * 1024;

const DEFAULT_USER_AGENT: &str = "oxi/0.6";
/// Zero-config search backends used when no SearXNG URL is set. DuckDuckGo's HTML endpoint
/// needs no API key, but it serves an anomaly-challenge page (HTTP 202) to clients that
/// don't look like a browser, so requests to it carry a full browser header set.
const DDG_HTML_URL: &str = "https://html.duckduckgo.com/html/";
/// Bing offers a stable, zero-config RSS feed of its search results at `?format=rss`. It is
/// plain XML (title/link/description per item), tolerant of any User-Agent, and not gated by a
/// bot-challenge page like DuckDuckGo's HTML endpoint, so it is the preferred zero-config
/// backend. Bing caps the feed at ~10 items regardless of a `count=` parameter; we just trim.
const BING_RSS_URL: &str = "https://www.bing.com/search";
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
            .tls_danger_accept_invalid_certs(accept_invalid_certs)
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

pub(crate) fn tool_web_search(
    base_url: &str,
    backend: crate::settings::WebSearchBackend,
    args: &Value,
) -> Result<String, String> {
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
        // No SearXNG configured: use exactly the backend the user picked — no fallback.
        // Surfacing its error directly makes misconfiguration visible instead of silently
        // masking it with another backend's results.
        return zero_config_search(query, count, backend);
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

/// Zero-config search with no API key or setup: use exactly the backend the user picked.
/// Bing is preferred because it serves plain XML with no bot-challenge page; DuckDuckGo's
/// HTML endpoint is currently blocked by an anomaly challenge. Whatever the selection, its
/// error is returned directly so the user sees what failed rather than another backend's
/// results masking the problem.
fn zero_config_search(
    query: &str,
    count: usize,
    backend: crate::settings::WebSearchBackend,
) -> Result<String, String> {
    use crate::settings::WebSearchBackend;
    match backend {
        WebSearchBackend::Bing => bing_search(query, count),
        WebSearchBackend::DuckDuckGo => ddg_search(query, count),
        WebSearchBackend::SearXng => Err(err(
            "SearXNG backend selected but no SearXNG URL is configured in Settings → Tools → Web search",
        )),
    }
}

/// Search Bing's RSS endpoint and format the results like the SearXNG path.
fn bing_search(query: &str, count: usize) -> Result<String, String> {
    let resp = http_get(
        BING_RSS_URL,
        &[("q", query), ("format", "rss")],
        false,
        DEFAULT_USER_AGENT,
        &[],
    )?;
    if resp.status >= 400 {
        return Err(format!("Bing returned HTTP {}", resp.status));
    }

    let results = parse_rss_items(&resp.body, count);
    if results.is_empty() {
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

/// Extract `(title, url, snippet)` triples from an RSS 2.0 feed (Bing's search feed Dieses).
/// String-based like the rest of the parsers: each `<item>…</item>` carries a `<title>`,
/// `<link>` and `<description>`. RSS puts the URL as element *text* (not an attribute), unlike
/// HTML, so we grab the text between the open and close tags directly.
fn parse_rss_items(xml: &str, count: usize) -> Vec<(String, String, String)> {
    let lower = xml.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut cursor = 0;
    while out.len() < count {
        let Some(rel) = lower[cursor..].find("<item>") else {
            break;
        };
        let start = cursor + rel + "<item>".len();
        let end = match lower[start..].find("</item>") {
            Some(r) => start + r,
            None => break,
        };
        cursor = end + "</item>".len();
        let block = &xml[start..end];
        let block_lower = &lower[start..end];

        let title = extract_tag_text(block, block_lower, "title")
            .map(clean_fragment)
            .filter(|s| !s.is_empty());
        let link = extract_tag_text(block, block_lower, "link")
            .map(clean_fragment)
            .filter(|s| !s.is_empty());
        let snippet = extract_tag_text(block, block_lower, "description")
            .map(clean_fragment)
            .unwrap_or_default();
        if let (Some(title), Some(link)) = (title, link) {
            out.push((title, link, snippet))
        }
    }
    out
}

/// Text between `<tag>…</tag>` for the first occurrence, with the entity-decoding deferred to
/// the caller. `lower` is the same slice lowercased so the tag search is case-insensitive
/// (RSS tags are lowercase in practice, but be lenient like the rest of the parsers).
fn extract_tag_text<'a>(html: &'a str, lower: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = lower.find(&open)? + open.len();
    let end = start + lower[start..].find(&close)?;
    Some(&html[start..end])
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
    if absolute.contains("duckduckgo.com/l/")
        && let Ok(parsed) = url::Url::parse(&absolute)
        && let Some((_, dest)) = parsed.query_pairs().find(|(k, _)| k == "uddg")
    {
        return dest.into_owned();
    }
    absolute
}

fn ip_is_private_or_special(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let o = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 192 && o[1] == 0 && o[2] == 2)
                || (o[0] == 198 && (o[1] == 18 || o[1] == 19))
                || (o[0] == 198 && o[1] == 51 && o[2] == 100)
                || (o[0] == 203 && o[1] == 0 && o[2] == 113)
        }
        IpAddr::V6(ip) => {
            let s = ip.segments();
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || ip.to_ipv4_mapped().is_some_and(|v4| ip_is_private_or_special(v4.into()))
        }
    }
}

fn public_addresses_for_url(url: &url::Url) -> Result<Vec<std::net::SocketAddr>, String> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(err("url must use http:// or https://"));
    }
    let host = url.host_str().ok_or_else(|| err("url is missing a host"))?;
    if host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".localhost")
        || host.ends_with(".local")
    {
        return Err(err("web_fetch blocks local/private destinations"));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| err("url has no usable port"))?;
    let addresses: Vec<_> = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("could not resolve {host}: {e}"))?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|a| ip_is_private_or_special(a.ip())) {
        return Err(err("web_fetch blocks local/private destinations"));
    }
    Ok(addresses)
}

fn public_http_get_bounded(url: &str) -> Result<HttpResponse, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("invalid url: {e}"))?;
    let initial_addresses = public_addresses_for_url(&parsed)?;
    let initial_host = parsed
        .host_str()
        .ok_or_else(|| err("url is missing a host"))?
        .to_string();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .user_agent(DEFAULT_USER_AGENT)
            // Pin the validated DNS result so a hostname cannot rebind to a private IP between
            // validation and connection. Redirects are disabled for the same reason.
            .resolve_to_addrs(&initial_host, &initial_addresses)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("client: {e}"))?;
        let resp = client
            .get(parsed)
            .send()
            .await
            .map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status().as_u16();
        if (300..400).contains(&status) {
            return Err(
                "web_fetch does not follow redirects; fetch the public destination URL directly"
                    .to_string(),
            );
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if let Some(len) = resp.content_length()
            && len > MAX_FETCH_BODY_BYTES as u64
        {
            return Err(format!(
                "response is too large ({len} bytes; limit is {MAX_FETCH_BODY_BYTES})"
            ));
        }
        let mut stream = resp.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| format!("read body: {e}"))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_FETCH_BODY_BYTES {
                return Err(format!(
                    "response exceeded the {MAX_FETCH_BODY_BYTES}-byte limit"
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let body = String::from_utf8_lossy(&bytes).into_owned();
        Ok(HttpResponse {
            status,
            content_type,
            body,
        })
    })
}

pub(crate) fn tool_web_fetch(args: &Value) -> Result<String, String> {
    let url = args
        .get("url")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("missing url"))?;
    let max_chars = args
        .get("max_chars")
        .and_then(|x| x.as_u64())
        .map(|n| (n as usize).min(MAX_TOOL_OUTPUT_CHARS))
        .unwrap_or(DEFAULT_FETCH_CHARS);

    let resp = public_http_get_bounded(url)?;
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
#[path = "web/tests.rs"]
mod tests;
