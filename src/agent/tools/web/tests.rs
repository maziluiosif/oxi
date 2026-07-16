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
fn web_fetch_rejects_loopback_and_private_hosts() {
    for url in [
        "http://localhost:8080/secret",
        "http://127.0.0.1/secret",
        "http://10.0.0.1/secret",
        "http://[::1]/secret",
        "http://169.254.169.254/latest/meta-data/",
    ] {
        let res = tool_web_fetch(&serde_json::json!({"url": url}));
        assert!(res.is_err(), "{url} should be blocked");
    }
}

#[test]
fn web_search_requires_query() {
    let res = tool_web_search(
        "https://example.invalid",
        crate::settings::WebSearchBackend::Bing,
        &serde_json::json!({}),
    );
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

#[test]
#[ignore = "hits the real DuckDuckGo endpoint"]
fn ddg_search_live() {
    let out = ddg_search("rust programming language", 5).expect("ddg_search failed");
    println!("{out}");
    assert!(out.contains("Search results for:"));
    assert!(out.contains("https://"));
}

#[test]
fn parse_rss_items_extracts_title_link_snippet() {
    let xml = concat!(
        "<rss version=\"2.0\"><channel>",
        "<item><title>The <b>Rust</b> Language</title>",
        "<link>https://www.rust-lang.org/</link>",
        "<description>Blazingly fast &amp; memory-efficient.</description></item>",
        "<item><title>Second</title><link>https://example.com</link></item>",
        "<item><link>https://no-title.example</link></item>",
        "</channel></rss>"
    );
    let results = parse_rss_items(xml, 10);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, "The Rust Language");
    assert_eq!(results[0].1, "https://www.rust-lang.org/");
    assert_eq!(results[0].2, "Blazingly fast & memory-efficient.");
    assert!(results[1].2.is_empty());
}

#[test]
fn parse_rss_items_respects_count() {
    let mut xml = String::from("<rss><channel>");
    for n in 0..15 {
        xml.push_str(&format!(
            "<item><title>r{n}</title><link>https://x{n}.example</link></item>"
        ));
    }
    xml.push_str("</channel></rss>");
    let results = parse_rss_items(&xml, 3);
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].0, "r0");
    assert_eq!(results[2].0, "r2");
}

#[test]
fn parse_rss_items_empty_feed_returns_nothing() {
    let xml = "<rss><channel></channel></rss>";
    assert!(parse_rss_items(xml, 10).is_empty());
    assert!(parse_rss_items("", 10).is_empty());
}

#[test]
#[ignore = "hits the real Bing endpoint"]
fn bing_search_live() {
    let out = bing_search("rust programming language", 5).expect("bing_search failed");
    println!("{out}");
    assert!(out.contains("Search results for:"));
    assert!(out.contains("https://"));
}

#[test]
#[ignore = "hits the real Bing endpoint"]
fn web_search_bing_zero_config_live() {
    use crate::settings::WebSearchBackend;
    let out = tool_web_search(
        "",
        WebSearchBackend::Bing,
        &serde_json::json!({"query": "rust programming language", "count": 5}),
    )
    .expect("zero-config web_search failed");
    println!("{out}");
    assert!(out.contains("Search results for:"));
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
