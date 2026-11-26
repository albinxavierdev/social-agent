//! Readability-style HTML extraction (no network). Used by the HTTP and Obscura extractors.

use chrono::{DateTime, NaiveDate, Utc};
use scraper::{ElementRef, Html, Selector};
use url::Url;

#[derive(Debug, Default, Clone)]
pub struct ParsedHtml {
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub language: Option<String>,
    pub canonical_url: Option<String>,
    pub og_title: Option<String>,
    pub og_description: Option<String>,
    pub images: Vec<String>,
    pub text: String,
    pub script_count: usize,
    pub has_spa_root: bool,
}

const MAX_TEXT_CHARS: usize = 120_000;
const MAX_IMAGES: usize = 12;

fn sel(s: &str) -> Selector {
    Selector::parse(s).expect("static selector")
}

fn meta(doc: &Html, key_attr: &str, key: &str) -> Option<String> {
    let s = sel(&format!("meta[{key_attr}=\"{key}\"]"));
    doc.select(&s).filter_map(|e| e.value().attr("content")).map(|c| c.trim().to_string()).find(|c| !c.is_empty())
}

pub fn clean_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    let s = s.trim();
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok().and_then(|d| d.and_hms_opt(0, 0, 0)).map(|d| d.and_utc()))
}

const SKIP_TAGS: &[&str] = &["script", "style", "noscript", "nav", "footer", "aside", "header", "form", "svg", "template", "iframe"];
/// Class/id tokens that mark page furniture rather than article prose (info-boxes, nav boxes, reference lists, ...).
const SKIP_CLASSES: &[&str] = &[
    "infobox",
    "navbox",
    "vertical-navbox",
    "sidebar",
    "toc",
    "reflist",
    "references",
    "mw-editsection",
    "noprint",
    "hatnote",
    "navigation",
    "breadcrumb",
    "breadcrumbs",
    "cookie-banner",
    "metadata",
    "thumbcaption",
    "catlinks",
    "printfooter",
];

fn is_furniture(el: &ElementRef, skip_tables: bool) -> bool {
    let v = el.value();
    SKIP_TAGS.contains(&v.name())
        || (skip_tables && v.name() == "table")
        || v.classes().any(|c| SKIP_CLASSES.contains(&c))
        || v.id().is_some_and(|i| SKIP_CLASSES.contains(&i))
}

fn in_skipped(el: &ElementRef, skip_tables: bool) -> bool {
    is_furniture(el, skip_tables) || el.ancestors().filter_map(ElementRef::wrap).any(|a| is_furniture(&a, skip_tables))
}

/// Remove footnote markers such as `[12]`, `[note 4]`, `[citation needed]` that clutter prose.
pub fn strip_citations(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('[') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find(']') {
            Some(j) if j <= 20 => {
                let inner = after[..j].trim().to_ascii_lowercase();
                let is_marker = (!inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()))
                    || inner == "citation needed"
                    || inner == "clarification needed"
                    || ["note ", "nb ", "a", "b", "c"]
                        .iter()
                        .any(|p| inner.strip_prefix(p).is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())));
                if is_marker {
                    rest = &after[j + 1..];
                } else {
                    out.push('[');
                    rest = after;
                }
            }
            _ => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

pub fn parse(html: &str, base: &Url) -> ParsedHtml {
    let doc = Html::parse_document(html);
    let mut out = ParsedHtml::default();

    out.title = doc.select(&sel("title")).next().map(|t| clean_ws(&t.text().collect::<String>())).filter(|t| !t.is_empty());
    out.og_title = meta(&doc, "property", "og:title");
    out.og_description = meta(&doc, "property", "og:description").or_else(|| meta(&doc, "name", "description"));
    out.author = meta(&doc, "name", "author").or_else(|| meta(&doc, "property", "article:author")).or_else(|| {
        doc.select(&sel("[rel=\"author\"], .author, [itemprop=\"author\"]"))
            .next()
            .map(|e| clean_ws(&e.text().collect::<String>()))
            .filter(|a| !a.is_empty() && a.len() < 120)
    });
    out.published_at = meta(&doc, "property", "article:published_time")
        .or_else(|| meta(&doc, "name", "date"))
        .or_else(|| meta(&doc, "itemprop", "datePublished"))
        .or_else(|| doc.select(&sel("time[datetime]")).next().and_then(|t| t.value().attr("datetime").map(str::to_owned)))
        .and_then(|s| parse_date(&s));
    out.language = doc
        .select(&sel("html"))
        .next()
        .and_then(|h| h.value().attr("lang"))
        .and_then(|l| l.split(['-', '_']).next())
        .map(|l| l.to_ascii_lowercase())
        .filter(|l| !l.is_empty());
    out.canonical_url = doc
        .select(&sel("link[rel=\"canonical\"]"))
        .next()
        .and_then(|l| l.value().attr("href"))
        .and_then(|h| base.join(h).ok())
        .map(|u| u.to_string())
        .or_else(|| meta(&doc, "property", "og:url"));

    // Images: og:image first, then in-article <img>.
    let mut images: Vec<String> = vec![];
    let mut push_img = |raw: &str| {
        if let Ok(u) = base.join(raw.trim()) {
            if matches!(u.scheme(), "http" | "https") {
                let s = u.to_string();
                if !images.contains(&s) && images.len() < MAX_IMAGES {
                    images.push(s);
                }
            }
        }
    };
    if let Some(og) = meta(&doc, "property", "og:image") {
        push_img(&og);
    }
    for img in doc.select(&sel("article img[src], main img[src]")) {
        if let Some(src) = img.value().attr("src") {
            push_img(src);
        }
    }
    out.images = images;

    out.script_count = doc.select(&sel("script")).count();
    out.has_spa_root = doc.select(&sel("#root, #__next, #app, #__nuxt, [data-reactroot]")).next().is_some();

    // Main text.
    let container = ["article", "main", "[role=\"main\"]", "body"]
        .iter()
        .filter_map(|s| doc.select(&sel(s)).next())
        .find(|c| clean_ws(&c.text().collect::<String>()).len() > 200)
        .or_else(|| doc.select(&sel("body")).next());

    let collect = |skip_tables: bool| -> Vec<String> {
        let mut blocks: Vec<String> = vec![];
        if let Some(c) = container {
            for el in c.select(&sel("h1,h2,h3,h4,h5,h6,p,li,blockquote,pre,figcaption")) {
                if in_skipped(&el, skip_tables) {
                    continue;
                }
                let t = clean_ws(&strip_citations(&el.text().collect::<String>()));
                if t.is_empty() || blocks.last() == Some(&t) {
                    continue;
                }
                blocks.push(t);
            }
        }
        blocks
    };
    // Tables are usually info-boxes/nav tables. Only fall back to them for old table-layout pages.
    let mut blocks = collect(true);
    if blocks.join(" ").chars().count() < 200 {
        blocks = collect(false);
    }
    let mut text = blocks.join("\n\n");
    if text.chars().count() > MAX_TEXT_CHARS {
        text = text.chars().take(MAX_TEXT_CHARS).collect();
    }
    out.text = text;
    out
}

/// `<meta http-equiv="refresh" content="0; url=...">` target, if it fires within 5 seconds.
/// Many sites (including moved blogs) serve these tiny stub pages instead of an HTTP redirect.
pub fn meta_refresh_target(html: &str, base: &Url) -> Option<Url> {
    let doc = Html::parse_document(html);
    for m in doc.select(&sel("meta[http-equiv]")) {
        if !m.value().attr("http-equiv").is_some_and(|v| v.eq_ignore_ascii_case("refresh")) {
            continue;
        }
        let content = m.value().attr("content")?;
        let (delay, rest) = content.split_once(';')?;
        if delay.trim().parse::<f64>().ok()? > 5.0 {
            continue;
        }
        let rest = rest.trim();
        let url_part = rest.get(..4).filter(|p| p.eq_ignore_ascii_case("url=")).map(|_| &rest[4..])?;
        return base.join(url_part.trim().trim_matches(|c| c == '\'' || c == '"')).ok();
    }
    None
}

/// First absolute http(s) string literal inside an inline script that performs a client-side redirect
/// (`location.replace(..)`, `location.href = ..`, `location.assign(..)`). Used only on near-empty stub pages.
pub fn js_redirect_target(html: &str, base: &Url) -> Option<Url> {
    let doc = Html::parse_document(html);
    for sc in doc.select(&sel("script")) {
        let code: String = sc.text().collect();
        if !(code.contains("location.replace") || code.contains("location.href") || code.contains("location.assign") || code.contains("window.location")) {
            continue;
        }
        let mut chars = code.char_indices().peekable();
        while let Some((i, c)) = chars.next() {
            if c == '"' || c == '\'' {
                let rest = &code[i + 1..];
                if let Some(end) = rest.find(c) {
                    let lit = &rest[..end];
                    if (lit.starts_with("http://") || lit.starts_with("https://")) && !lit.contains(char::is_whitespace) {
                        return base.join(lit).ok();
                    }
                    // skip past this literal
                    while chars.peek().is_some_and(|(j, _)| *j <= i + 1 + end) {
                        chars.next();
                    }
                }
            }
        }
    }
    None
}

/// Where a near-empty "redirect stub" page sends the visitor: meta refresh (also inside `<noscript>`) or a JS redirect.
pub fn redirect_stub_target(html: &str, base: &Url) -> Option<Url> {
    if let Some(u) = meta_refresh_target(html, base) {
        return Some(u);
    }
    let doc = Html::parse_document(html);
    for n in doc.select(&sel("noscript")) {
        let inner: String = n.text().collect();
        if let Some(u) = meta_refresh_target(&inner, base) {
            return Some(u);
        }
    }
    js_redirect_target(html, base)
}

/// Short lead summary derived without an LLM (LLM summaries live in the report step).
pub fn lead_summary(text: &str, description: Option<&str>) -> String {
    if let Some(d) = description.map(clean_ws).filter(|d| d.len() > 40) {
        return truncate_chars(&d, 300);
    }
    truncate_chars(&clean_ws(text), 300)
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    match cut.rfind(['.', '!', '?']) {
        Some(i) if i > max / 2 => cut[..=i].to_string(),
        _ => format!("{}…", cut.trim_end()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"<!doctype html><html lang="en-US"><head>
        <title> My Post </title>
        <meta property="og:title" content="OG Title"><meta property="og:description" content="A description that is definitely longer than forty characters.">
        <meta property="og:image" content="/img/hero.png"><meta name="author" content="Jane Doe">
        <meta property="article:published_time" content="2026-11-20T10:00:00Z">
        <link rel="canonical" href="/blog/my-post"></head><body>
        <nav><p>Menu junk</p></nav>
        <article><h1>Heading</h1><p>First paragraph with enough words to count as real content in this article body.</p>
        <p>Second paragraph that continues the story and adds more detail for the reader to enjoy fully today.</p>
        <aside><p>Ad junk</p></aside><script>var x=1</script></article><footer><p>Footer junk</p></footer></body></html>"#;

    #[test]
    fn extracts_metadata_and_main_text() {
        let base = Url::parse("https://example.com/blog/my-post?x=1").unwrap();
        let p = parse(PAGE, &base);
        assert_eq!(p.title.as_deref(), Some("My Post"));
        assert_eq!(p.og_title.as_deref(), Some("OG Title"));
        assert_eq!(p.author.as_deref(), Some("Jane Doe"));
        assert_eq!(p.language.as_deref(), Some("en"));
        assert_eq!(p.canonical_url.as_deref(), Some("https://example.com/blog/my-post"));
        assert_eq!(p.images, vec!["https://example.com/img/hero.png"]);
        assert_eq!(p.published_at.unwrap().to_rfc3339(), "2026-11-20T10:00:00+00:00");
        assert!(p.text.contains("First paragraph"));
        assert!(p.text.contains("Second paragraph"));
        assert!(!p.text.contains("junk"), "boilerplate must be stripped: {}", p.text);
    }

    #[test]
    fn skips_infoboxes_navboxes_and_citation_markers() {
        let html = r#"<html><body><main><div class="mw-body">
<table class="infobox"><tr><td><ul><li>Multi-paradigm: concurrent</li><li>functional</li></ul></td></tr></table>
<p class="hatnote">For other uses, see Rust.</p>
<p>Rust is a general-purpose programming language that emphasizes performance, type safety, and concurrency.[12] It enforces memory safety.[note 4]</p>
<p>It was designed by Graydon Hoare and first released publicly as a stable version in 2015 after years of work.[citation needed]</p>
<div class="navbox"><ul><li>Programming languages navigation junk</li></ul></div>
<ol class="references"><li>Reference entry that should not be prose</li></ol>
</div></main></body></html>"#;
        let p = parse(html, &Url::parse("https://example.com/wiki/Rust").unwrap());
        assert!(p.text.starts_with("Rust is a general-purpose programming language"), "lead paragraph comes first: {:?}", &p.text[..60.min(p.text.len())]);
        for junk in ["Multi-paradigm", "navigation junk", "Reference entry", "For other uses", "[12]", "[note 4]", "[citation needed]"] {
            assert!(!p.text.contains(junk), "{junk} leaked into the text: {}", p.text);
        }
        assert!(p.text.contains("memory safety") && p.text.contains("Graydon Hoare"));
    }

    #[test]
    fn table_layout_pages_still_extract() {
        let html = r#"<html><body><table><tr><td><p>An old-school page whose whole body lives in a table layout, with enough words to count as real content for the extractor to keep.</p><p>A second paragraph keeps going so that the total comfortably passes the minimum length threshold used for the fallback.</p></td></tr></table></body></html>"#;
        let p = parse(html, &Url::parse("https://old.example.com/").unwrap());
        assert!(p.text.contains("old-school page") && p.text.contains("second paragraph"), "{}", p.text);
    }

    #[test]
    fn citation_stripping_keeps_real_brackets() {
        assert_eq!(strip_citations("a[1] b[note 2] c[citation needed] d"), "a b c d");
        assert_eq!(strip_citations("array[index] and [see below]"), "array[index] and [see below]");
        assert_eq!(strip_citations("unclosed [ bracket"), "unclosed [ bracket");
    }

    #[test]
    fn detects_spa_shell() {
        let html = r#"<html><body><div id="root"></div><script src="a.js"></script><script src="b.js"></script></body></html>"#;
        let p = parse(html, &Url::parse("https://x.com/").unwrap());
        assert!(p.has_spa_root);
        assert_eq!(p.script_count, 2);
        assert!(p.text.len() < 50);
    }

    #[test]
    fn finds_meta_refresh_targets() {
        let base = Url::parse("https://old.example.com/a/b").unwrap();
        let t = |h: &str| meta_refresh_target(h, &base).map(|u| u.to_string());
        assert_eq!(t(r#"<meta http-equiv="refresh" content="0; url=https://new.example.com/x">"#).as_deref(), Some("https://new.example.com/x"));
        assert_eq!(t(r#"<meta http-equiv="Refresh" content="1;URL='/moved'">"#).as_deref(), Some("https://old.example.com/moved"));
        assert_eq!(t(r#"<meta http-equiv="refresh" content="30; url=/slow">"#), None, "slow refreshes are page features, not redirects");
        assert_eq!(t(r#"<meta http-equiv="refresh" content="5">"#), None);
        assert_eq!(t("<html></html>"), None);
    }

    #[test]
    fn finds_js_and_noscript_redirects_like_the_rust_blog_stub() {
        let base = Url::parse("https://blog.rust-lang.org/2024/05/02/Rust-1.78.0.html").unwrap();
        let stub = r#"<!doctype html><meta charset="utf-8"><title>Redirect</title>
<script>
  const target = "https://blog.rust-lang.org/2024/05/02/Rust-1.78.0/";
  const hash = window.location.hash || "";
  window.location.replace(target + hash);
</script>
<noscript><meta http-equiv="refresh" content="0; url=https://blog.rust-lang.org/2024/05/02/Rust-1.78.0/"></noscript>
<p><a href="https://blog.rust-lang.org/2024/05/02/Rust-1.78.0/">Click here</a> to be redirected.</p>"#;
        let want = Some("https://blog.rust-lang.org/2024/05/02/Rust-1.78.0/".to_string());
        assert_eq!(redirect_stub_target(stub, &base).map(|u| u.to_string()), want);
        assert_eq!(js_redirect_target(stub, &base).map(|u| u.to_string()), want, "JS alone is enough");
        let js_only = r#"<script>window.location.href = '/new/place';</script>"#;
        assert_eq!(js_redirect_target(js_only, &base), None, "relative-only literals are ignored (never guessed)");
        assert_eq!(js_redirect_target("<script>var x = \"https://a.example/x\";</script>", &base), None, "no redirect call, no redirect");
    }

    #[test]
    fn summary_prefers_description() {
        let s = lead_summary("body text", Some("A description that is definitely longer than forty characters."));
        assert!(s.starts_with("A description"));
        assert!(truncate_chars(&"word ".repeat(200), 50).chars().count() <= 52);
    }
}
