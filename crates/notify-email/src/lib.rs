use anyhow::{Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyAction {
    PermissionDecision {
        request_id: String,
        approved: bool,
        message: Option<String>,
    },
    DirectiveReply {
        cycle_id: String,
        text: String,
    },
}

pub struct SendEmailRequest<'a> {
    pub smtp_host: &'a str,
    pub smtp_port: u16,
    pub from: &'a str,
    pub to: &'a str,
    pub password: Option<&'a str>,
    pub subject: &'a str,
    pub body: &'a str,
    pub cycle_id: Option<&'a str>,
    pub html_override: Option<&'a str>,
}

pub async fn send_email(request: SendEmailRequest<'_>) -> Result<()> {
    use lettre::message::header::ContentType;
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

    let html_body = match request.html_override {
        Some(html) => html.to_string(),
        None => markdown_to_html_email(request.body),
    };

    let mut builder = Message::builder()
        .from(request.from.parse()?)
        .to(request.to.parse()?)
        .subject(request.subject)
        .header(ContentType::TEXT_HTML);

    if let Some(cid) = request.cycle_id {
        let msg_id = format!("<ambient-{}@horon-agent>", cid);
        builder = builder.message_id(Some(msg_id));
    }

    let email = builder.body(html_body)?;

    let mut transport_builder =
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(request.smtp_host)?.port(request.smtp_port);

    if let Some(pw) = request.password {
        transport_builder = transport_builder.credentials(Credentials::new(request.from.to_string(), pw.to_string()));
    }

    let transport = transport_builder.build();
    transport.send(email).await?;
    Ok(())
}

/// TLS stream type returned by [`connect_imap`]. `imap::Client` needs a concrete type so it can
/// apply its own buffering on top.
type ImapTlsStream = rustls::StreamOwned<rustls::ClientConnection, std::net::TcpStream>;

/// A server that accepts TCP but never answers must not pin the blocking task forever. The old
/// `imap::ClientBuilder` path used the default OS timeout (none for reads), so this is a new bound.
const IMAP_IO_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Root store for IMAP TLS: the bundled webpki roots first, then whatever the host's store offers.
///
/// Bundled roots keep mail notifications working on hosts without `ca-certificates`; the system
/// store is layered on top so private CAs keep working where an operator installed them.
fn imap_root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    // Certificates the system store ships but rustls cannot parse as a trust anchor are skipped
    // inside this call, so one malformed entry does not disable every other root.
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);

    roots
}

fn imap_tls_config() -> Result<std::sync::Arc<rustls::ClientConfig>> {
    let config =
        rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
            .with_safe_default_protocol_versions()
            .context("IMAP TLS configuration")?
            .with_root_certificates(imap_root_store())
            .with_no_client_auth();
    Ok(std::sync::Arc::new(config))
}

/// Wrap a connected socket in TLS and finish the handshake.
fn imap_tls_stream(
    host: &str,
    tcp: std::net::TcpStream,
    config: std::sync::Arc<rustls::ClientConfig>,
) -> Result<ImapTlsStream> {
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .with_context(|| format!("invalid IMAP server name {host:?}"))?;
    let connection = rustls::ClientConnection::new(config, server_name).context("IMAP TLS handshake")?;
    let mut stream = rustls::StreamOwned::new(connection, tcp);
    // Handshake is driven lazily by the first read; run it now so a certificate or protocol
    // failure is reported as a handshake error rather than as a bogus greeting.
    stream
        .conn
        .complete_io(&mut stream.sock)
        .context("IMAP TLS handshake")?;
    Ok(stream)
}

fn imap_tcp(host: &str, port: u16) -> Result<std::net::TcpStream> {
    let tcp = std::net::TcpStream::connect((host, port)).with_context(|| format!("IMAP connect to {host}:{port}"))?;
    tcp.set_read_timeout(Some(IMAP_IO_TIMEOUT))?;
    tcp.set_write_timeout(Some(IMAP_IO_TIMEOUT))?;
    Ok(tcp)
}

/// Upper bound for a single IMAP response line. Servers send short lines; this only stops a
/// broken peer from growing our buffer without bound.
const IMAP_MAX_LINE: usize = 8 * 1024;

/// Upper bound for the number of response lines read for one command.
const IMAP_MAX_LINES: usize = 256;

/// Result of looking at one line while waiting for the tagged completion of a command.
#[derive(Debug, PartialEq, Eq)]
enum TaggedReply {
    /// The line tagged with our tag reported OK.
    Ok,
    /// The line tagged with our tag reported NO, BAD or BYE (or an unknown status).
    Failed(String),
    /// An untagged line (or one tagged by somebody else); keep reading.
    Incomplete,
}

/// Classify one response line against the tag we sent.
fn classify_tagged_line(tag: &str, line: &str) -> TaggedReply {
    let Some(rest) = line.strip_prefix(tag).and_then(|rest| rest.strip_prefix(' ')) else {
        return TaggedReply::Incomplete;
    };

    let status = rest.split_whitespace().next().unwrap_or("");
    if status.eq_ignore_ascii_case("OK") {
        TaggedReply::Ok
    } else {
        TaggedReply::Failed(line.to_string())
    }
}

/// Read one CRLF-terminated line from a plain socket, one byte at a time.
///
/// Reading exactly one line matters: after the STARTTLS reply the socket switches to TLS, so any
/// byte already pulled into a user-space buffer would be lost from the TLS record stream.
fn read_imap_line<S: std::io::Read>(stream: &mut S) -> std::io::Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                if line.is_empty() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "IMAP connection closed before a response line",
                    ));
                }
                break;
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                if byte[0] != b'\r' {
                    line.push(byte[0]);
                }
                if line.len() > IMAP_MAX_LINE {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "IMAP response line too long",
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

/// Send one tagged command and read until its tagged reply, returning every line seen including
/// the tagged one. Fails on NO/BAD/BYE and on EOF before the tagged reply.
fn imap_command<S: std::io::Read + std::io::Write>(stream: &mut S, tag: &str, command: &str) -> Result<Vec<String>> {
    let phase = format!("IMAP {command}");
    stream
        .write_all(format!("{tag} {command}\r\n").as_bytes())
        .with_context(|| phase.clone())?;
    stream.flush().with_context(|| phase.clone())?;

    let mut lines = Vec::new();
    loop {
        if lines.len() >= IMAP_MAX_LINES {
            return Err(anyhow::anyhow!("{phase}: too many response lines"));
        }
        let line = read_imap_line(stream).with_context(|| phase.clone())?;
        let verdict = classify_tagged_line(tag, &line);
        lines.push(line);
        match verdict {
            TaggedReply::Ok => return Ok(lines),
            TaggedReply::Failed(line) => {
                return Err(anyhow::anyhow!("{phase} rejected: {line}"));
            }
            TaggedReply::Incomplete => continue,
        }
    }
}

/// Upgrade a plain IMAP socket to TLS with STARTTLS.
///
/// This mirrors what `imap::ClientBuilder`'s `AutoTls` mode did for non-993 ports: read the
/// greeting, require the server to advertise STARTTLS, run it, and fail otherwise.
fn imap_starttls(
    host: &str,
    mut tcp: std::net::TcpStream,
    config: std::sync::Arc<rustls::ClientConfig>,
) -> Result<ImapTlsStream> {
    let greeting = read_imap_line(&mut tcp).context("IMAP greeting")?;
    if !greeting.starts_with('*') {
        return Err(anyhow::anyhow!("IMAP STARTTLS: unexpected greeting {greeting:?}"));
    }

    let capabilities = imap_command(&mut tcp, "h1", "CAPABILITY")?;
    // IMAP keywords are case-insensitive, so match the untagged CAPABILITY data case-insensitively
    // (RFC 3501 section 9 formally upper-cases them, but servers vary).
    let advertises_starttls = capabilities.iter().any(|line| {
        let mut tokens = line.split_whitespace();
        matches!(tokens.next(), Some("*"))
            && tokens
                .next()
                .is_some_and(|keyword| keyword.eq_ignore_ascii_case("CAPABILITY"))
            && line
                .split_whitespace()
                .any(|token| token.eq_ignore_ascii_case("STARTTLS"))
    });
    if !advertises_starttls {
        return Err(anyhow::anyhow!("IMAP server {host} does not advertise STARTTLS"));
    }

    imap_command(&mut tcp, "h2", "STARTTLS")?;
    imap_tls_stream(host, tcp, config)
}

/// Connect the way `imap::ClientBuilder`'s `AutoTls` mode used to: implicit TLS on 993,
/// STARTTLS everywhere else (and a hard failure when the server does not advertise STARTTLS).
fn connect_imap(host: &str, port: u16) -> Result<imap::Client<ImapTlsStream>> {
    let config = imap_tls_config()?;
    let tcp = imap_tcp(host, port)?;

    if port == 993 {
        let mut client = imap::Client::new(imap_tls_stream(host, tcp, config)?);
        client.read_greeting().context("IMAP greeting")?;
        return Ok(client);
    }

    let stream = imap_starttls(host, tcp, config)?;
    let mut client = imap::Client::new(stream);
    // STARTTLS does not re-send a greeting, so the upgraded connection is already past it.
    client.greeting_read = true;
    Ok(client)
}

pub fn poll_imap_once(host: &str, port: u16, user: &str, pass: &str) -> Result<Vec<ReplyAction>> {
    let client = connect_imap(host, port)?;
    let mut session = client
        .login(user, pass)
        .map_err(|(e, _)| anyhow::anyhow!("IMAP login failed: {}", e))?;

    session.select("INBOX")?;

    let reply_search = session.search("UNSEEN HEADER In-Reply-To \"@horon-agent>\"")?;
    let button_search = session.search("UNSEEN SUBJECT \"[horon-agent-perm:\"")?;

    let mut all_seqs: Vec<_> = reply_search.into_iter().chain(button_search).collect();
    all_seqs.sort_unstable();
    all_seqs.dedup();

    if all_seqs.is_empty() {
        session.logout()?;
        return Ok(Vec::new());
    }

    let seq_set: String = all_seqs.iter().map(|s| s.to_string()).collect::<Vec<_>>().join(",");

    let mut actions = Vec::new();
    let messages = session.fetch(&seq_set, "RFC822")?;
    for message in messages.iter() {
        if let Some(body) = message.body()
            && let Some(parsed) = mail_parser::MessageParser::default().parse(body)
        {
            let in_reply_to = parsed.in_reply_to().as_text().unwrap_or("").to_string();
            let subject = parsed.subject().unwrap_or("");

            let cycle_id = if in_reply_to.contains("@horon-agent>") {
                in_reply_to
                    .trim_start_matches("<ambient-")
                    .trim_end_matches("@horon-agent>")
                    .to_string()
            } else if let Some(start) = subject.find("[horon-agent-perm:") {
                let rest = &subject[start + "[horon-agent-perm:".len()..];
                rest.split(']').next().unwrap_or("").to_string()
            } else {
                continue;
            };

            let body_text = parsed.body_text(0).map(|s| strip_quoted_reply(&s)).unwrap_or_default();

            let effective_text = if body_text.trim().is_empty() {
                subject.to_string()
            } else {
                body_text
            };

            if effective_text.trim().is_empty() {
                continue;
            }

            if cycle_id.starts_with("req_") {
                let (approved, message) = parse_permission_reply(effective_text.trim());
                actions.push(ReplyAction::PermissionDecision {
                    request_id: cycle_id,
                    approved,
                    message,
                });
            } else {
                actions.push(ReplyAction::DirectiveReply {
                    cycle_id,
                    text: effective_text.trim().to_string(),
                });
            }
        }
    }

    session.store(&seq_set, "+FLAGS (\\Seen)")?;
    session.logout()?;
    Ok(actions)
}

pub fn extract_permission_id(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for word in lower.split_whitespace() {
        if word.starts_with("req_") {
            return Some(word.to_string());
        }
    }
    None
}

pub fn parse_permission_reply(text: &str) -> (bool, Option<String>) {
    let lower = text.to_lowercase();
    let first_line = lower.lines().next().unwrap_or("").trim();

    let approve_words = ["approve", "approved", "yes", "lgtm", "go ahead", "ok", "sure"];
    let deny_words = ["deny", "denied", "no", "reject", "rejected", "stop", "nope"];

    let has_approve = approve_words.iter().any(|w| first_line.contains(w));
    let has_deny = deny_words.iter().any(|w| first_line.contains(w));
    let approved = has_approve && !has_deny;

    let message = if text.trim().len() > 20 {
        Some(text.trim().to_string())
    } else {
        None
    };

    (approved, message)
}

pub fn build_permission_email_html(action: &str, description: &str, request_id: &str, reply_to: &str) -> String {
    let now = chrono::Utc::now();
    let timestamp = now.format("%Y-%m-%d %H:%M:%S UTC").to_string();

    let approve_subj_raw = format!("[horon-agent-perm:{}] Approved", request_id);
    let deny_subj_raw = format!("[horon-agent-perm:{}] Denied", request_id);
    let approve_subject = urlencoding::encode(&approve_subj_raw);
    let deny_subject = urlencoding::encode(&deny_subj_raw);
    let approve_body = urlencoding::encode("Approved");
    let deny_body = urlencoding::encode("Denied");

    let approve_href = format!("mailto:{}?subject={}&body={}", reply_to, approve_subject, approve_body);
    let deny_href = format!("mailto:{}?subject={}&body={}", reply_to, deny_subject, deny_body);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  body {{
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    color: #1a1a1a;
    line-height: 1.6;
    max-width: 640px;
    margin: 0 auto;
    padding: 20px;
    background: #f5f5f5;
  }}
  .container {{
    background: #ffffff;
    border-radius: 8px;
    padding: 24px 28px;
    border: 1px solid #e0e0e0;
  }}
  h1 {{
    font-size: 1.3em;
    color: #2d2d2d;
    border-bottom: 2px solid #f59e0b;
    padding-bottom: 6px;
    margin-top: 0;
  }}
  .field {{
    margin-bottom: 12px;
  }}
  .field-label {{
    font-weight: 600;
    color: #555;
    font-size: 0.85em;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }}
  .field-value {{
    margin-top: 2px;
    color: #1a1a1a;
  }}
  .request-id {{
    font-family: monospace;
    background: #f0f0f0;
    padding: 2px 6px;
    border-radius: 3px;
    font-size: 0.85em;
  }}
  .buttons {{
    margin-top: 24px;
    text-align: center;
  }}
  .btn {{
    display: inline-block;
    padding: 12px 32px;
    border-radius: 6px;
    text-decoration: none;
    font-weight: 600;
    font-size: 1em;
    margin: 0 8px;
  }}
  .btn-approve {{
    background: #22c55e;
    color: #ffffff;
  }}
  .btn-deny {{
    background: #ef4444;
    color: #ffffff;
  }}
  .timestamp {{
    margin-top: 16px;
    font-size: 0.8em;
    color: #888;
  }}
  .hint {{
    margin-top: 8px;
    font-size: 0.8em;
    color: #999;
    font-style: italic;
  }}
  .footer {{
    margin-top: 20px;
    padding-top: 12px;
    border-top: 1px solid #e0e0e0;
    font-size: 0.8em;
    color: #888;
  }}
</style>
</head>
<body>
<div class="container">
  <h1>Permission Request</h1>
  <div class="field">
    <div class="field-label">Action</div>
    <div class="field-value"><strong>{action}</strong></div>
  </div>
  <div class="field">
    <div class="field-label">Description</div>
    <div class="field-value">{description}</div>
  </div>
  <div class="field">
    <div class="field-label">Request ID</div>
    <div class="field-value"><span class="request-id">{request_id}</span></div>
  </div>
  <div class="buttons">
    <a href="{approve_href}" class="btn btn-approve">Approve</a>
    <a href="{deny_href}" class="btn btn-deny">Deny</a>
  </div>
  <div class="hint">Clicking opens a pre-filled email — just hit Send.</div>
  <div class="hint">Or reply to this email with "Approved" or "Denied".</div>
  <div class="timestamp">Sent at {timestamp}</div>
</div>
<div class="footer">
  Sent by horon-agent ambient mode
</div>
</body>
</html>"#
    )
}

fn markdown_to_html_email(markdown: &str) -> String {
    use pulldown_cmark::{Options, Parser, html};

    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);

    let parser = Parser::new_ext(markdown, options);
    let mut html_content = String::new();
    html::push_html(&mut html_content, parser);

    format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
  body {{
    font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
    color: #1a1a1a;
    line-height: 1.6;
    max-width: 640px;
    margin: 0 auto;
    padding: 20px;
    background: #f5f5f5;
  }}
  .container {{
    background: #ffffff;
    border-radius: 8px;
    padding: 24px 28px;
    border: 1px solid #e0e0e0;
  }}
  h1, h2, h3 {{
    color: #2d2d2d;
    margin-top: 1.2em;
    margin-bottom: 0.4em;
  }}
  h1 {{ font-size: 1.3em; border-bottom: 2px solid #6366f1; padding-bottom: 6px; }}
  h2 {{ font-size: 1.1em; }}
  strong {{ color: #111; }}
  ul, ol {{ padding-left: 1.4em; }}
  li {{ margin-bottom: 4px; }}
  code {{
    background: #f0f0f0;
    padding: 2px 5px;
    border-radius: 3px;
    font-size: 0.9em;
  }}
  pre {{
    background: #1e1e2e;
    color: #cdd6f4;
    padding: 12px 16px;
    border-radius: 6px;
    overflow-x: auto;
    font-size: 0.85em;
  }}
  pre code {{
    background: none;
    padding: 0;
    color: inherit;
  }}
  table {{
    border-collapse: collapse;
    width: 100%;
    margin: 1em 0;
  }}
  th, td {{
    border: 1px solid #ddd;
    padding: 6px 10px;
    text-align: left;
  }}
  th {{ background: #f8f8f8; font-weight: 600; }}
  .footer {{
    margin-top: 20px;
    padding-top: 12px;
    border-top: 1px solid #e0e0e0;
    font-size: 0.8em;
    color: #888;
  }}
</style>
</head>
<body>
<div class="container">
{html_content}
</div>
<div class="footer">
  Sent by horon-agent ambient mode
</div>
</body>
</html>"#
    )
}

fn strip_quoted_reply(text: &str) -> String {
    text.lines()
        .take_while(|line| {
            let trimmed = line.trim();
            !trimmed.starts_with('>') && trimmed != "--" && trimmed != "-- " && !trimmed.starts_with("On ")
                || trimmed.is_empty()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal socket stand-in: a fixed response script plus a write log. It mimics a real stream
    /// where writes never rewind the read side (a `Cursor<Vec<u8>>` would).
    struct ScriptedStream {
        input: Vec<u8>,
        pos: usize,
        written: Vec<u8>,
    }

    impl ScriptedStream {
        fn new(input: &[u8]) -> Self {
            Self {
                input: input.to_vec(),
                pos: 0,
                written: Vec::new(),
            }
        }

        fn unread(&self) -> &[u8] {
            &self.input[self.pos..]
        }
    }

    impl std::io::Read for ScriptedStream {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let available = &self.input[self.pos..];
            if available.is_empty() || buf.is_empty() {
                return Ok(0);
            }
            let n = available.len().min(buf.len());
            buf[..n].copy_from_slice(&available[..n]);
            self.pos += n;
            Ok(n)
        }
    }

    impl std::io::Write for ScriptedStream {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_classify_tagged_line_ok() {
        assert_eq!(classify_tagged_line("a1", "a1 OK STARTTLS completed"), TaggedReply::Ok);
        assert_eq!(classify_tagged_line("a2", "a2 ok"), TaggedReply::Ok);
    }

    #[test]
    fn test_classify_tagged_line_failures() {
        assert_eq!(
            classify_tagged_line("a1", "a1 NO STARTTLS not supported"),
            TaggedReply::Failed("a1 NO STARTTLS not supported".to_string())
        );
        assert_eq!(
            classify_tagged_line("a1", "a1 BAD unknown command"),
            TaggedReply::Failed("a1 BAD unknown command".to_string())
        );
        assert_eq!(
            classify_tagged_line("a1", "a1 BYE shutting down"),
            TaggedReply::Failed("a1 BYE shutting down".to_string())
        );
    }

    #[test]
    fn test_classify_tagged_line_ignores_untagged_and_foreign_tags() {
        assert_eq!(
            classify_tagged_line("a1", "* CAPABILITY IMAP4rev1 STARTTLS"),
            TaggedReply::Incomplete
        );
        assert_eq!(classify_tagged_line("a1", "* OK still here"), TaggedReply::Incomplete);
        assert_eq!(classify_tagged_line("a1", "a10 OK not ours"), TaggedReply::Incomplete);
        assert_eq!(
            classify_tagged_line("a1", "a1OK missing separator"),
            TaggedReply::Incomplete
        );
        assert_eq!(classify_tagged_line("a1", "a1"), TaggedReply::Incomplete);
    }

    #[test]
    fn test_read_imap_line_handles_crlf_and_eof() {
        let mut input = &b"* OK ready\r\n* BYE\r\n"[..];
        assert_eq!(read_imap_line(&mut input).unwrap(), "* OK ready");
        assert_eq!(read_imap_line(&mut input).unwrap(), "* BYE");
    }

    #[test]
    fn test_read_imap_line_eof_without_line() {
        let mut input = &b""[..];
        let err = read_imap_line(&mut input).expect_err("empty stream must not look like a line");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn test_read_imap_line_returns_trailing_line_without_newline() {
        let mut input = &b"a1 OK done"[..];
        assert_eq!(read_imap_line(&mut input).unwrap(), "a1 OK done");
    }

    #[test]
    fn test_imap_command_stops_at_tagged_ok_after_untagged() {
        let mut stream = ScriptedStream::new(b"* OK hi\r\na1 OK STARTTLS go ahead\r\n");
        let lines = imap_command(&mut stream, "a1", "STARTTLS").unwrap();
        assert_eq!(lines, vec!["* OK hi", "a1 OK STARTTLS go ahead"]);
        // Reading stopped at the tagged reply, so nothing past it was consumed.
        assert_eq!(stream.unread(), b"");
    }

    #[test]
    fn test_imap_command_sends_tagged_line() {
        let mut stream = ScriptedStream::new(b"a7 OK done\r\n");
        let lines = imap_command(&mut stream, "a7", "STARTTLS").unwrap();
        assert_eq!(lines, vec!["a7 OK done"]);
        assert_eq!(stream.written, b"a7 STARTTLS\r\n");
    }

    #[test]
    fn test_imap_command_reports_no_and_eof() {
        let mut no = ScriptedStream::new(b"a1 NO nope\r\n");
        let err = imap_command(&mut no, "a1", "STARTTLS").unwrap_err();
        assert!(format!("{err:#}").contains("IMAP STARTTLS rejected"));

        let mut eof = ScriptedStream::new(b"* OK hi\r\n");
        let err = imap_command(&mut eof, "a1", "STARTTLS").unwrap_err();
        assert!(format!("{err:#}").contains("IMAP STARTTLS"));
    }

    #[test]
    fn test_markdown_to_html_email() {
        let md = "**Ambient Cycle Summary:**\n\n- Cleaned 3 memories\n- Status: Complete\n";
        let html = markdown_to_html_email(md);
        assert!(html.contains("<strong>Ambient Cycle Summary:</strong>"));
        assert!(html.contains("<li>"));
        assert!(html.contains("horon-agent ambient mode"));
    }

    #[test]
    fn test_strip_quoted_reply() {
        let email = "Thanks, please clean up the test data.\n\n> On Mon, Feb 9, 2026 horon-agent wrote:\n> Ambient cycle complete.\n";
        let stripped = strip_quoted_reply(email);
        assert!(stripped.contains("clean up the test data"));
        assert!(!stripped.contains("Ambient cycle complete"));
    }

    #[test]
    fn test_strip_quoted_reply_signature() {
        let email = "Focus on memory gardening.\n--\nAlice\n";
        let stripped = strip_quoted_reply(email);
        assert!(stripped.contains("Focus on memory gardening"));
        assert!(!stripped.contains("Alice"));
    }

    #[test]
    fn test_parse_permission_reply_approve() {
        let (approved, _) = parse_permission_reply("Yes, go ahead");
        assert!(approved);
        let (approved, _) = parse_permission_reply("Approved");
        assert!(approved);
        let (approved, _) = parse_permission_reply("LGTM");
        assert!(approved);
        let (approved, _) = parse_permission_reply("sure thing");
        assert!(approved);
        let (approved, _) = parse_permission_reply("ok");
        assert!(approved);
    }

    #[test]
    fn test_parse_permission_reply_deny() {
        let (approved, _) = parse_permission_reply("No, too risky");
        assert!(!approved);
        let (approved, _) = parse_permission_reply("Denied");
        assert!(!approved);
        let (approved, _) = parse_permission_reply("reject this");
        assert!(!approved);
        let (approved, _) = parse_permission_reply("nope");
        assert!(!approved);
        let (approved, _) = parse_permission_reply("Stop, don't do that");
        assert!(!approved);
    }

    #[test]
    fn test_parse_permission_reply_ambiguous_defaults_deny() {
        let (approved, _) = parse_permission_reply("hmm let me think about it");
        assert!(!approved);
        let (approved, _) = parse_permission_reply("");
        assert!(!approved);
    }

    #[test]
    fn test_parse_permission_reply_message() {
        let (_, message) = parse_permission_reply("yes");
        assert!(message.is_none());

        let (_, message) = parse_permission_reply("Approved, but please use a feature branch for this");
        assert!(message.is_some());
    }

    #[test]
    fn test_extract_permission_id() {
        assert_eq!(extract_permission_id("approve req_123"), Some("req_123".to_string()));
        assert_eq!(extract_permission_id("nothing here"), None);
    }

    #[test]
    fn test_build_permission_email_html() {
        let html = build_permission_email_html("apply patch", "Touch Cargo.toml", "req_123", "horon-agent@example.com");
        assert!(html.contains("Permission Request"));
        assert!(html.contains("req_123"));
        assert!(html.contains("mailto:horon-agent@example.com"));
    }
}
