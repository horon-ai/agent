use super::{Tool, ToolContext, ToolEffects, ToolOutput, artifacts};
use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const MAX_SIZE: usize = 5 * 1024 * 1024; // 5MB
/// Context budget for the text handed back to the model. Full pages routinely
/// exceed 150 KB (~40k tokens); the rest is saved as an `artifact://`.
const WEBFETCH_OUTPUT_BUDGET_BYTES: usize = 40_000;
/// Links whose target exceeds this length are rendered as their anchor text
/// only. Long URLs are typically encoded payloads (pre-filled editors, tracking
/// parameters, data URIs) whose cost far exceeds their navigational value.
const MAX_URL_CHARS: usize = 300;
const DEFAULT_TIMEOUT: u64 = 30;
const MAX_TIMEOUT: u64 = 120;

/// Fetches a URL and returns its text.
///
/// The HTTP client is not a field: constructing it parses the entire TLS trust
/// store (449 anchors in this build), and the registry is built at `serve`
/// startup, so holding a client here charges every daemon start for work only a
/// real fetch needs. `provider::shared_http_client()` is already a process-wide
/// `OnceLock`, so asking for it at the call site builds it on first request and
/// shares it with every other tool.
const MAX_REDIRECTS: usize = 10;

fn is_disallowed_ipv4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 0 // 0.0.0.0/8 ("this network")
        || o[0] == 10 // 10.0.0.0/8 (RFC 1918)
        || (o[0] == 100 && (o[1] & 0b1100_0000) == 64) // 100.64.0.0/10 (CGNAT / cloud metadata)
        || o[0] == 127 // 127.0.0.0/8 (loopback)
        || (o[0] == 169 && o[1] == 254) // 169.254.0.0/16 (link-local / IMDS)
        || (o[0] == 172 && (16..=31).contains(&o[1])) // 172.16.0.0/12 (RFC 1918)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24 (IETF protocol assignments)
        || (o[0] == 192 && o[1] == 0 && o[2] == 2) // 192.0.2.0/24 (TEST-NET-1)
        || (o[0] == 192 && o[1] == 88 && o[2] == 99) // 192.88.99.0/24 (retired 6to4)
        || (o[0] == 192 && o[1] == 168) // 192.168.0.0/16 (RFC 1918)
        || (o[0] == 198 && (18..=19).contains(&o[1])) // 198.18.0.0/15 (benchmarking)
        || (o[0] == 198 && o[1] == 51 && o[2] == 100) // 198.51.100.0/24 (TEST-NET-2)
        || (o[0] == 203 && o[1] == 0 && o[2] == 113) // 203.0.113.0/24 (TEST-NET-3)
        || o[0] >= 224 // 224.0.0.0/4 multicast + 240.0.0.0/4 reserved + 255.255.255.255 broadcast
}

fn is_disallowed_ipv6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return is_disallowed_ipv4(mapped);
    }
    let s = ip.segments();
    // IPv4-compatible retired ::/96 (also covers :: and ::1).
    if s[..6] == [0; 6] {
        return true;
    }
    // NAT64 (64:ff9b::/96 and 64:ff9b:1::/48) embedding an internal IPv4 address.
    if s[0] == 0x0064 && s[1] == 0xff9b {
        let embedded = Ipv4Addr::from(((s[6] as u32) << 16) | (s[7] as u32));
        if is_disallowed_ipv4(embedded) {
            return true;
        }
    }
    // 6to4 (2002::/16) embedding an internal IPv4 address.
    if s[0] == 0x2002 {
        let embedded = Ipv4Addr::from(((s[1] as u32) << 16) | (s[2] as u32));
        if is_disallowed_ipv4(embedded) {
            return true;
        }
    }
    ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local() // fc00::/7
        || ip.is_unicast_link_local() // fe80::/10
        || (s[0] & 0xffc0) == 0xfec0 // fec0::/10 retired site-local
        || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        || (s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0) // 100::/64 discard
}

pub(crate) fn is_disallowed_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_disallowed_ipv4(v4),
        IpAddr::V6(v6) => is_disallowed_ipv6(v6),
    }
}

fn is_blocked_hostname(host: &str) -> bool {
    let normalized = host.trim_end_matches('.').to_ascii_lowercase();
    normalized.is_empty()
        || normalized == "localhost"
        || normalized.ends_with(".localhost")
        || normalized.ends_with(".local")
        || normalized.ends_with(".internal")
        || normalized == "ip6-localhost"
        || normalized == "ip6-loopback"
}

fn validate_url_syntax(raw: &str) -> Result<reqwest::Url> {
    if !raw.starts_with("http://") && !raw.starts_with("https://") {
        return Err(anyhow::anyhow!("URL must start with http:// or https://"));
    }
    let parsed = reqwest::Url::parse(raw).context("Invalid URL")?;
    anyhow::ensure!(
        matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some(),
        "URL must be an absolute http:// or https:// URL"
    );
    Ok(parsed)
}

async fn validate_public_target(url: &reqwest::Url) -> Result<()> {
    match url.host() {
        Some(url::Host::Ipv4(v4)) => {
            if is_disallowed_ipv4(v4) {
                return Err(anyhow::anyhow!(
                    "Blocked non-public IPv4 target `{v4}` (loopback, private, link-local, or reserved)"
                ));
            }
            Ok(())
        }
        Some(url::Host::Ipv6(v6)) => {
            if is_disallowed_ipv6(v6) {
                return Err(anyhow::anyhow!(
                    "Blocked non-public IPv6 target `{v6}` (loopback, private, link-local, or reserved)"
                ));
            }
            Ok(())
        }
        Some(url::Host::Domain(domain)) => {
            if is_blocked_hostname(domain) {
                return Err(anyhow::anyhow!("Blocked local or internal hostname `{domain}`"));
            }
            if let Ok(ip) = domain.parse::<IpAddr>() {
                if is_disallowed_ip(ip) {
                    return Err(anyhow::anyhow!("Blocked non-public IP target `{ip}`"));
                }
                return Ok(());
            }
            let port = url.port_or_known_default().unwrap_or(80);
            if let Ok(resolved) = tokio::net::lookup_host((domain, port)).await {
                let addrs: Vec<SocketAddr> = resolved.collect();
                if let Some(disallowed) = addrs.iter().find(|addr| is_disallowed_ip(addr.ip())) {
                    return Err(anyhow::anyhow!(
                        "Blocked target `{domain}` resolving to non-public address {}",
                        disallowed.ip()
                    ));
                }
            }
            Ok(())
        }
        None => Err(anyhow::anyhow!("URL must include a host")),
    }
}

struct PublicOnlyDnsResolver;

impl reqwest::dns::Resolve for PublicOnlyDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            if is_blocked_hostname(&host) {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!("Blocked local or internal hostname `{host}`"),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }
            let resolved = tokio::net::lookup_host((host.as_str(), 0)).await?;
            let addrs: Vec<SocketAddr> = resolved.collect();
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("Host `{host}` resolved to no addresses"),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }
            if let Some(disallowed) = addrs.iter().find(|addr| is_disallowed_ip(addr.ip())) {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    format!(
                        "Blocked target `{host}` resolving to non-public address {}",
                        disallowed.ip()
                    ),
                )) as Box<dyn std::error::Error + Send + Sync>);
            }
            let iter: reqwest::dns::Addrs = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

struct WebFetchClientInitError(String);

fn webfetch_http_client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<std::result::Result<reqwest::Client, WebFetchClientInitError>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .use_preconfigured_tls(horon_agent_platform::tls_client_config().as_ref().clone())
            .redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(PublicOnlyDnsResolver))
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|err| WebFetchClientInitError(format!("Failed to initialize webfetch HTTP client: {err}")))
    }) {
        Ok(client) => Ok(client),
        Err(err) => Err(anyhow::anyhow!(err.0.clone())),
    }
}

async fn send_validated_get(
    client: &reqwest::Client,
    initial: reqwest::Url,
    timeout: u64,
    validate: impl Fn(&reqwest::Url) -> futures::future::BoxFuture<'static, Result<()>>,
) -> Result<(reqwest::Url, reqwest::Response)> {
    let mut current = initial;
    for _hop in 0..=MAX_REDIRECTS {
        validate(&current).await?;
        let response = client
            .get(current.clone())
            .header(reqwest::header::USER_AGENT, "Mozilla/5.0 (compatible; HoronAgent/1.0)")
            .timeout(Duration::from_secs(timeout))
            .send()
            .await?;

        if response.status().is_redirection() {
            let Some(location) = response.headers().get(reqwest::header::LOCATION) else {
                return Ok((current, response));
            };
            let location = location
                .to_str()
                .context("Redirect Location header is not valid UTF-8")?;
            let next = current.join(location).context("Invalid redirect Location URL")?;
            anyhow::ensure!(
                matches!(next.scheme(), "http" | "https"),
                "Blocked redirect to unsupported URL scheme `{}`",
                next.scheme()
            );
            current = next;
            continue;
        }
        return Ok((current, response));
    }
    Err(anyhow::anyhow!("Too many redirects (max {MAX_REDIRECTS})"))
}

pub struct WebFetchTool;

impl WebFetchTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct WebFetchInput {
    url: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    timeout: Option<u64>,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read().union(ToolEffects::network())
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "intent": super::intent_schema_property(),
                "url": {
                    "type": "string",
                    "description": "URL."
                },
                "format": {
                    "type": "string",
                    "enum": ["text", "markdown", "html"],
                    "description": "Output format."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: WebFetchInput = serde_json::from_value(input)?;

        // Validate URL and SSRF target before opening any socket.
        let initial_url = validate_url_syntax(&params.url)?;

        let timeout = params.timeout.unwrap_or(DEFAULT_TIMEOUT).min(MAX_TIMEOUT);
        let format = params.format.as_deref().unwrap_or("markdown");

        let client = webfetch_http_client()?;
        let (_final_url, response) = send_validated_get(client, initial_url, timeout, |url| {
            let url = url.clone();
            Box::pin(async move { validate_public_target(&url).await })
        })
        .await?;

        let status = response.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!("HTTP error: {}", status));
        }

        // Check content length
        if let Some(len) = response.content_length()
            && len as usize > MAX_SIZE
        {
            return Err(anyhow::anyhow!(
                "Response too large: {} bytes (max {} bytes)",
                len,
                MAX_SIZE
            ));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let mut body_bytes = Vec::new();
        let mut truncated = false;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let remaining = MAX_SIZE.saturating_sub(body_bytes.len());
            if chunk.len() > remaining {
                body_bytes.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body_bytes.extend_from_slice(&chunk);
        }

        let mut body = String::from_utf8_lossy(&body_bytes).into_owned();
        if truncated {
            body.push_str(&format!("...\n\n(truncated, showing first {} bytes)", MAX_SIZE));
        }

        // Format output
        let output = match format {
            "html" => body,
            "text" => html_to_text(&body),
            "markdown" => {
                if content_type.contains("text/html") {
                    html_to_markdown(&body)
                } else {
                    body
                }
            }
            _ => {
                if content_type.contains("text/html") {
                    html_to_markdown(&body)
                } else {
                    body
                }
            }
        };

        let full_len = output.len();
        let output = artifacts::condense_for_context(&ctx.session_id, "webfetch", output, WEBFETCH_OUTPUT_BUDGET_BYTES);

        Ok(ToolOutput::new(format!(
            "Fetched {} ({} bytes)\n\n{}",
            params.url, full_len, output
        )))
    }
}

mod html_regex {
    use regex::Regex;
    use std::sync::OnceLock;

    fn compile_regex(pattern: &str, label: &str) -> Option<Regex> {
        match Regex::new(pattern) {
            Ok(regex) => Some(regex),
            Err(err) => {
                crate::logging::warn(&format!("webfetch: failed to compile static regex {label}: {}", err));
                None
            }
        }
    }

    macro_rules! static_regex {
        ($name:ident, $pat:expr_2021) => {
            pub fn $name() -> Option<&'static Regex> {
                static RE: OnceLock<Option<Regex>> = OnceLock::new();
                RE.get_or_init(|| compile_regex($pat, stringify!($name))).as_ref()
            }
        };
    }

    static_regex!(script, r"(?is)<script[^>]*>.*?</script>");
    static_regex!(style, r"(?is)<style[^>]*>.*?</style>");
    // Match attribute values (which may themselves contain `>`) before falling
    // back to bare `>`-terminated content, so tags carrying JSON payloads such as
    // Parsoid's `data-mw` do not leak their contents into the output.
    static_regex!(
        tag,
        r#"(?s)</?[A-Za-z!/][^\s/>]*(?:\s+[^\s=/>]+(?:\s*=\s*(?:"[^"]*"|'[^']*'|[^\s>]*))?)*\s*/?>"#
    );
    static_regex!(whitespace, r"\n\s*\n\s*\n");
    // Runs of empty markdown list items left behind after tag stripping.
    static_regex!(empty_bullets, r"(?m)^[ \t]*-[ \t]*$\n?");

    /// HTML elements whose content is non-prose by specification: navigation,
    /// complementary/tangential content, interactive controls, and embedded
    /// non-text resources. This is deliberately limited to elements whose *spec
    /// definition* excludes primary content, so it generalizes across sites
    /// rather than encoding any single site's markup.
    ///
    /// Notably excludes `<header>`, which commonly wraps the article `<h1>`,
    /// byline, and publication date, and `<footer>`, which can carry
    /// article-level attribution when nested inside `<article>`.
    const CHROME_TAGS: [&str; 10] = [
        "nav", "aside", "form", "noscript", "svg", "iframe", "template", "select", "dialog", "canvas",
    ];

    static CHROME: OnceLock<Vec<Regex>> = OnceLock::new();

    pub fn chrome() -> &'static [Regex] {
        CHROME.get_or_init(|| {
            CHROME_TAGS
                .iter()
                .filter_map(|tag| compile_regex(&format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}\s*>"), "chrome"))
                .collect()
        })
    }
    static_regex!(link, r#"(?i)<a[^>]*href=["']([^"']+)["'][^>]*>([^<]*)</a>"#);
    static_regex!(strong, r"(?i)<(?:strong|b)>([^<]*)</(?:strong|b)>");
    static_regex!(em, r"(?i)<(?:em|i)>([^<]*)</(?:em|i)>");
    static_regex!(code, r"(?i)<code>([^<]*)</code>");
    static_regex!(pre_code, r"(?is)<pre[^>]*><code[^>]*>(.+?)</code></pre>");
    static_regex!(li, r"(?i)<li[^>]*>");
    // HTML comments frequently contain build metadata, conditional markup, and
    // commented-out blocks, none of which are rendered content.
    static_regex!(comment, r"(?s)<!--.*?-->");

    static H_OPEN: OnceLock<Option<[Regex; 6]>> = OnceLock::new();
    static H_CLOSE: OnceLock<Option<[Regex; 6]>> = OnceLock::new();

    pub fn h_open() -> Option<&'static [Regex; 6]> {
        H_OPEN
            .get_or_init(|| {
                let mut compiled = Vec::with_capacity(6);
                for i in 0..6 {
                    let pattern = format!(r"(?i)<h{}[^>]*>", i + 1);
                    compiled.push(compile_regex(&pattern, "heading open")?);
                }
                compiled.try_into().ok()
            })
            .as_ref()
    }

    pub fn h_close() -> Option<&'static [Regex; 6]> {
        H_CLOSE
            .get_or_init(|| {
                let mut compiled = Vec::with_capacity(6);
                for i in 0..6 {
                    let pattern = format!(r"(?i)</h{}>", i + 1);
                    compiled.push(compile_regex(&pattern, "heading close")?);
                }
                compiled.try_into().ok()
            })
            .as_ref()
    }
}

fn html_to_text(html: &str) -> String {
    let mut text = html.to_string();

    let (Some(script), Some(style), Some(tag), Some(whitespace)) = (
        html_regex::script(),
        html_regex::style(),
        html_regex::tag(),
        html_regex::whitespace(),
    ) else {
        return html.trim().to_string();
    };

    text = script.replace_all(&text, "").to_string();
    text = style.replace_all(&text, "").to_string();
    if let Some(comment) = html_regex::comment() {
        text = comment.replace_all(&text, "").to_string();
    }
    for re in html_regex::chrome() {
        text = re.replace_all(&text, "").to_string();
    }

    text = text.replace("<br>", "\n");
    text = text.replace("<br/>", "\n");
    text = text.replace("<br />", "\n");
    text = text.replace("</p>", "\n\n");
    text = text.replace("</div>", "\n");
    text = text.replace("</li>", "\n");
    text = text.replace("</tr>", "\n");

    text = tag.replace_all(&text, "").to_string();

    text = text.replace("&nbsp;", " ");
    text = text.replace("&lt;", "<");
    text = text.replace("&gt;", ">");
    text = text.replace("&amp;", "&");
    text = text.replace("&quot;", "\"");
    text = text.replace("&#39;", "'");

    text = whitespace.replace_all(&text, "\n\n").to_string();

    text.trim().to_string()
}

/// Render one anchor as markdown, dropping targets that cost more context than
/// they convey.
///
/// Three general cases, none specific to any site:
/// - Empty anchor text means the link is a bare icon or control. Emitting
///   `[](url)` conveys nothing, so the whole link is dropped.
/// - Overlong targets are encoded payloads rather than addresses; the anchor
///   text is kept and the target dropped.
/// - Pure in-page fragments (`#foo`) are navigation aids with no destination
///   content, so the text is kept and the target dropped.
fn render_link(href: &str, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let href = href.trim();
    if href.is_empty() || href.starts_with('#') || href.chars().count() > MAX_URL_CHARS {
        return text.to_string();
    }
    format!("[{text}]({href})")
}

fn html_to_markdown(html: &str) -> String {
    let mut md = html.to_string();

    let (
        Some(script),
        Some(style),
        Some(link),
        Some(strong),
        Some(em),
        Some(code),
        Some(pre_code),
        Some(li),
        Some(tag),
        Some(whitespace),
    ) = (
        html_regex::script(),
        html_regex::style(),
        html_regex::link(),
        html_regex::strong(),
        html_regex::em(),
        html_regex::code(),
        html_regex::pre_code(),
        html_regex::li(),
        html_regex::tag(),
        html_regex::whitespace(),
    )
    else {
        return html.trim().to_string();
    };

    md = script.replace_all(&md, "").to_string();
    md = style.replace_all(&md, "").to_string();
    if let Some(comment) = html_regex::comment() {
        md = comment.replace_all(&md, "").to_string();
    }
    for re in html_regex::chrome() {
        md = re.replace_all(&md, "").to_string();
    }

    if let (Some(h_open), Some(h_close)) = (html_regex::h_open(), html_regex::h_close()) {
        for i in 0..6 {
            let prefix = "#".repeat(i + 1);
            md = h_open[i].replace_all(&md, &format!("\n{} ", prefix)).to_string();
            md = h_close[i].replace_all(&md, "\n").to_string();
        }
    }

    md = link
        .replace_all(&md, |caps: &regex::Captures<'_>| {
            render_link(
                caps.get(1).map_or("", |m| m.as_str()),
                caps.get(2).map_or("", |m| m.as_str()),
            )
        })
        .to_string();
    md = strong.replace_all(&md, "**$1**").to_string();
    md = em.replace_all(&md, "*$1*").to_string();
    md = code.replace_all(&md, "`$1`").to_string();
    md = pre_code.replace_all(&md, "\n```\n$1\n```\n").to_string();
    md = li.replace_all(&md, "\n- ").to_string();

    md = md.replace("<br>", "\n");
    md = md.replace("<br/>", "\n");
    md = md.replace("<br />", "\n");
    md = md.replace("</p>", "\n\n");

    md = tag.replace_all(&md, "").to_string();

    md = md.replace("&nbsp;", " ");
    md = md.replace("&lt;", "<");
    md = md.replace("&gt;", ">");
    md = md.replace("&amp;", "&");
    md = md.replace("&quot;", "\"");
    md = md.replace("&#39;", "'");

    if let Some(empty_bullets) = html_regex::empty_bullets() {
        md = empty_bullets.replace_all(&md, "").to_string();
    }
    md = whitespace.replace_all(&md, "\n\n").to_string();

    md.trim().to_string()
}

#[cfg(test)]
#[path = "webfetch_corpus_tests.rs"]
mod corpus_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_non_prose_elements() {
        let html = "<nav><a href='/x'>Menu</a></nav><p>Body text</p>\
                    <aside>Related</aside><form><select><option>Pick</option></select></form>";
        let md = html_to_markdown(html);
        assert!(md.contains("Body text"));
        assert!(!md.contains("Menu"), "nav should be dropped: {md}");
        assert!(!md.contains("Related"), "aside should be dropped: {md}");
        assert!(!md.contains("Pick"), "form controls should be dropped: {md}");
    }

    #[test]
    fn keeps_article_header_and_footer_content() {
        // <header> usually holds the title/byline and <footer> can hold
        // article attribution, so neither is treated as chrome.
        let html = "<article><header><h1>Real Title</h1><p>By Author</p></header>\
                    <p>Body</p><footer>Published 2026</footer></article>";
        let md = html_to_markdown(html);
        for needle in ["Real Title", "By Author", "Body", "Published 2026"] {
            assert!(md.contains(needle), "{needle} missing from {md}");
        }
    }

    #[test]
    fn drops_empty_links_and_overlong_targets() {
        assert_eq!(render_link("https://example.com", ""), "");
        assert_eq!(render_link("#section", "Jump"), "Jump");
        let long = format!("https://example.com/?code={}", "a".repeat(MAX_URL_CHARS));
        assert_eq!(render_link(&long, "Run"), "Run");
        assert_eq!(
            render_link("https://example.com", "Home"),
            "[Home](https://example.com)"
        );
    }

    #[test]
    fn strips_html_comments() {
        let md = html_to_markdown("<p>Keep</p><!-- build:12345 drop me -->");
        assert!(md.contains("Keep"));
        assert!(!md.contains("drop me"), "comment retained: {md}");
    }

    #[test]
    fn does_not_leak_attributes_containing_angle_brackets() {
        // Parsoid-style tags embed JSON in attributes; a naive `<[^>]+>` regex
        // stops at the first `>` inside the value and dumps the rest as text.
        let html = r#"<span data-mw='{"wt":"[[a]] > [[b]]"}'>Visible</span>"#;
        let text = html_to_text(html);
        assert_eq!(text, "Visible");
    }

    #[tokio::test]
    async fn blocks_loopback_private_metadata_and_obfuscated_ssrf_targets() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncWriteExt as _;

        // Bind a local listener so we can prove zero HTTP requests leave the tool.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_bg = Arc::clone(&hits);
        let accept_task = tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                hits_bg.fetch_add(1, Ordering::SeqCst);
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nSECRET")
                    .await;
            }
        });

        let tool = WebFetchTool::new();
        let ctx = ToolContext {
            session_id: "ssrf-test".into(),
            message_id: "m".into(),
            tool_call_id: "c".into(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: super::super::ToolExecutionMode::Direct,
        };

        let blocked_urls = [
            format!("http://127.0.0.1:{port}/control/requests"),
            format!("http://localhost:{port}/control/requests"),
            format!("http://sub.localhost:{port}/"),
            format!("http://[::1]:{port}/"),
            format!("http://[::ffff:127.0.0.1]:{port}/"),
            format!("http://2130706433:{port}/"), // decimal 127.0.0.1
            format!("http://0x7f000001:{port}/"), // hex 127.0.0.1
            "http://169.254.169.254/latest/meta-data/".to_string(),
            "http://metadata.google.internal/computeMetadata/v1/".to_string(),
            "http://10.0.0.1/".to_string(),
            "http://172.16.0.1/".to_string(),
            "http://192.168.1.1/".to_string(),
            "http://100.100.100.200/latest/meta-data/".to_string(),
            "http://[fe80::1]/".to_string(),
            "http://[fd00::1]/".to_string(),
            "http://[64:ff9b::127.0.0.1]/".to_string(),
            "http://0.0.0.0:8080/".to_string(),
        ];

        for target in &blocked_urls {
            let err = tool
                .execute(json!({"url": target}), ctx.clone())
                .await
                .expect_err(&format!("expected SSRF block for {target}"));
            let msg = err.to_string();
            assert!(
                msg.contains("Blocked") || msg.contains("private") || msg.contains("local"),
                "unexpected error for {target}: {msg}"
            );
        }
        assert_eq!(
            hits.load(Ordering::SeqCst),
            0,
            "local listener received unexpected SSRF traffic"
        );
        accept_task.abort();
    }

    #[tokio::test]
    async fn blocks_redirect_from_allowed_origin_to_private_target() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let redirector = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let redirect_port = redirector.local_addr().unwrap().port();
        let secret = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let secret_port = secret.local_addr().unwrap().port();
        let secret_hits = Arc::new(AtomicUsize::new(0));
        let secret_hits_bg = Arc::clone(&secret_hits);

        let redirect_task = tokio::spawn(async move {
            if let Ok((mut sock, _)) = redirector.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254:{secret_port}/meta\r\nContent-Length: 0\r\n\r\n"
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            }
        });
        let secret_task = tokio::spawn(async move {
            while let Ok((mut sock, _)) = secret.accept().await {
                secret_hits_bg.fetch_add(1, Ordering::SeqCst);
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nSECRET")
                    .await;
            }
        });

        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let initial = validate_url_syntax(&format!("http://127.0.0.1:{redirect_port}/start")).unwrap();
        let start_url = initial.clone();
        // Allow ONLY the simulated initial external server hop so the redirect hop exercises validate_public_target.
        let err = send_validated_get(&client, initial, 5, move |url| {
            let url = url.clone();
            let allowed = start_url.clone();
            Box::pin(async move {
                if url == allowed {
                    Ok(())
                } else {
                    validate_public_target(&url).await
                }
            })
        })
        .await
        .expect_err("redirect to metadata IP must be blocked");
        assert!(err.to_string().contains("169.254.169.254"), "{err}");
        assert_eq!(secret_hits.load(Ordering::SeqCst), 0);
        redirect_task.abort();
        secret_task.abort();
    }

    #[test]
    fn allows_public_ip_addresses() {
        assert!(!is_disallowed_ip("93.184.216.34".parse().unwrap()));
        assert!(!is_disallowed_ip("1.1.1.1".parse().unwrap()));
        assert!(!is_disallowed_ip("8.8.8.8".parse().unwrap()));
        assert!(!is_disallowed_ip("2606:4700:4700::1111".parse().unwrap()));
    }
}
