//! The opt-in TCP listener: a bearer credential before any request.
//!
//! The local socket, the named pipe and stdio keep their exact handshake. A TCP
//! connection carries one extra line first — `Authorization: Bearer <token>` —
//! and then nothing else changes: the same `hello` and the same frames follow.
//! A connection that fails, stalls or sends something else is closed before the
//! daemon is dialled and before any request is read, and nothing is written
//! back, so an unauthenticated peer cannot tell a wrong token from a missing
//! record and cannot use the listener as a probe.

use crate::credentials::{AuthenticatedCredential, CredentialStore};
use crate::run_bridge_stream_with_deadline;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use tokio::net::TcpListener;

/// Largest first line a TCP peer may send before authentication.
pub const MAX_AUTH_FRAME_BYTES: usize = 1024;
/// How long a TCP peer may take to send the authentication line.
pub const AUTH_DEADLINE: Duration = Duration::from_secs(10);

/// Why a connection was closed before authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthFailure {
    /// The peer closed (or reset) the connection before a complete line.
    Closed,
    /// No complete line arrived within the deadline.
    TimedOut,
    /// The first line was longer than [`MAX_AUTH_FRAME_BYTES`].
    Oversized,
    /// The first line is not `Authorization: Bearer <token>`.
    Malformed,
    /// No stored credential matches the presented token.
    NotRecognised,
    /// The credential store is missing, unsafe or unreadable: fail closed.
    Store,
}

impl AuthFailure {
    /// A log-safe description. It never contains the presented token.
    fn reason(self) -> &'static str {
        match self {
            Self::Closed => "the peer closed the connection",
            Self::TimedOut => "no authorization line arrived before the deadline",
            Self::Oversized => "the first line exceeded the pre-auth byte limit",
            Self::Malformed => "the first line is not `Authorization: Bearer <token>`",
            Self::NotRecognised => "no credential matches the presented token",
            Self::Store => "the credential store is unavailable or untrusted",
        }
    }
}

/// Read the pre-authentication line and verify it against the store.
pub(crate) async fn authenticate<R: AsyncRead + Unpin>(
    reader: &mut R,
    store: &CredentialStore,
    deadline: Duration,
) -> Result<AuthenticatedCredential, AuthFailure> {
    let line = match tokio::time::timeout(deadline, read_auth_line(reader)).await {
        Ok(Ok(line)) => line,
        Ok(Err(failure)) => return Err(failure),
        Err(_) => return Err(AuthFailure::TimedOut),
    };
    let Ok(text) = std::str::from_utf8(&line) else {
        return Err(AuthFailure::Malformed);
    };
    let token = parse_authorization_line(text).ok_or(AuthFailure::Malformed)?;
    match store.authenticate(token) {
        Ok(Some(authenticated)) => Ok(authenticated),
        Ok(None) => Err(AuthFailure::NotRecognised),
        Err(_) => Err(AuthFailure::Store),
    }
}

/// Read exactly one newline-terminated line without consuming a byte of the
/// next frame: the hello the client sends after authenticating must survive.
async fn read_auth_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Vec<u8>, AuthFailure> {
    let mut line = Vec::new();
    loop {
        let byte = match reader.read_u8().await {
            Ok(byte) => byte,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Err(AuthFailure::Closed),
            Err(_) => return Err(AuthFailure::Closed),
        };
        if byte == b'\n' {
            return Ok(line);
        }
        if line.len() >= MAX_AUTH_FRAME_BYTES {
            return Err(AuthFailure::Oversized);
        }
        line.push(byte);
    }
}

/// The token of an `Authorization: Bearer <token>` line.
///
/// The header name and the scheme are case-insensitive, as HTTP header names
/// are; the token itself is this build's 64-hex-character shape, which the
/// credential store re-checks before hashing.
fn parse_authorization_line(line: &str) -> Option<&str> {
    let (name, value) = line.trim_end_matches(['\r', '\n']).split_once(':')?;
    if !name.trim().eq_ignore_ascii_case("authorization") {
        return None;
    }
    horon_agent_base::gateway::parse_bearer_token(value.trim())
}

/// Serve one accepted TCP connection, or close it before reading any request.
pub(crate) async fn serve_client<S>(
    stream: S,
    legacy_socket: PathBuf,
    store: CredentialStore,
    auth_deadline: Duration,
    write_deadline: Duration,
    tenant: Option<String>,
) -> anyhow::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut read_half, write_half) = tokio::io::split(stream);
    match authenticate(&mut read_half, &store, auth_deadline).await {
        Ok(credential) => {
            let namespace = match tenant.as_deref() {
                Some(tenant) => format!(" tenant={tenant}"),
                None => String::new(),
            };
            eprintln!(
                "harness API bridge: authenticated credential {} ({}){namespace} on the exposed listener",
                credential.id, credential.label
            );
            run_bridge_stream_with_deadline(read_half, write_half, legacy_socket, write_deadline).await
        }
        Err(failure) => {
            eprintln!("harness API bridge: refused a TCP connection: {}", failure.reason());
            Ok(())
        }
    }
}

/// Accept TCP connections until the listener fails.
///
/// The caller owns the process lifetime: this loop never ends on idleness or on
/// the spawning process disappearing, which is the documented behaviour of
/// `--api-bind`.
pub(crate) async fn accept_loop(
    listener: TcpListener,
    legacy_socket: PathBuf,
    store: CredentialStore,
    auth_deadline: Duration,
    write_deadline: Duration,
    tenant: Option<String>,
) -> anyhow::Result<()> {
    loop {
        let (stream, peer) = listener.accept().await?;
        if let Err(error) = stream.set_nodelay(true) {
            eprintln!("harness API bridge: cannot disable Nagle for {peer}: {error}");
        }
        let legacy = legacy_socket.clone();
        let store = store.clone();
        let tenant = tenant.clone();
        tokio::spawn(async move {
            if let Err(error) = serve_client(stream, legacy, store, auth_deadline, write_deadline, tenant).await {
                eprintln!("harness API bridge: TCP client {peer} ended: {error:#}");
            }
            crate::allocator::trim();
        });
    }
}

/// The listener address as an operator should read it.
pub(crate) fn describe(bind: std::net::SocketAddr, insecure_behind_proxy: bool, tenant: Option<&str>) -> String {
    let proxy = if bind.ip().is_loopback() {
        "loopback only"
    } else if insecure_behind_proxy {
        "a TLS-terminating proxy must front this address"
    } else {
        "unexpected: a non-loopback address without the proxy acknowledgement"
    };
    match tenant {
        Some(tenant) => format!("tcp://{bind} (bearer token required, {proxy}, tenant={tenant})"),
        None => format!("tcp://{bind} (bearer token required, {proxy})"),
    }
}

#[cfg(test)]
#[path = "bridge_auth_tests.rs"]
mod tests;
