//! Native stdio server transport for remote SSH clients.
//!
//! A remote client runs `ssh <host> horon-agent server stdio`, which lands in
//! `run_stdio` and bridges its stdin/stdout to the local daemon socket. Each
//! frame carries the native Request/ServerEvent protocol, not the SDK harness
//! API. Closing this bridge closes only this connection, never the shared
//! daemon.

use anyhow::{Context, Result, bail};
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

const PROTOCOL: u32 = 1;
const HANDSHAKE_LIMIT: usize = 8192;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Serialize)]
pub(crate) struct NativeHandshake {
    pub kind: String,
    pub protocol: u32,
    pub version: String,
    pub working_dir: String,
    pub socket_path: String,
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let mut frame = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            bail!("remote closed before native SSH handshake");
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.map_or(available.len(), |n| n + 1);
        if frame.len() + count > HANDSHAKE_LIMIT {
            bail!("native SSH handshake exceeds {HANDSHAKE_LIMIT} bytes");
        }
        frame.extend_from_slice(&available[..count]);
        reader.consume(count);
        if newline.is_some() {
            break;
        }
    }
    Ok(frame)
}

async fn verify_daemon_protocol<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    tokio::time::timeout(STARTUP_TIMEOUT, async {
        writer.write_all(b"{\"type\":\"ping\",\"id\":0}\n").await?;
        writer.flush().await?;
        let frame = read_bounded_line(reader).await?;
        let pong: serde_json::Value = serde_json::from_slice(&frame)?;
        if pong["type"] != "pong" || pong["id"].as_u64() != Some(0)
            || pong["native_ssh_protocol"].as_u64() != Some(u64::from(PROTOCOL)) {
            bail!("remote daemon does not support native SSH protocol {PROTOCOL}; update/reload the remote HoronAgent server or select a matching daemon socket");
        }
        Ok::<_, anyhow::Error>(())
    }).await.context("remote daemon native SSH capability handshake timed out")??;
    Ok(())
}

/// CLI-only stdin/stdout bridge to an already-running persistent native server.
pub(crate) async fn run_stdio(socket: PathBuf) -> Result<()> {
    use std::io::Write;
    // Tokio stdin is uncancellable and can keep runtime shutdown alive after
    // daemon death while SSH still holds stdin open. A plain thread cannot.
    let (input, mut writer) = std::os::unix::net::UnixStream::pair()?;
    input.set_nonblocking(true)?;
    let input = UnixStream::from_std(input)?;
    std::thread::Builder::new()
        .name("native-ssh-stdin".into())
        .spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut writer);
            let _ = writer.flush();
            let _ = writer.shutdown(std::net::Shutdown::Write);
        })?;
    bridge_stream(input, tokio::io::stdout(), socket).await
}

async fn bridge_stream<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    mut input: R,
    mut output: W,
    socket: PathBuf,
) -> Result<()> {
    let daemon = tokio::time::timeout(STARTUP_TIMEOUT, UnixStream::connect(&socket))
        .await
        .context("native daemon connection timed out")??;
    let (read, mut write) = daemon.into_split();
    let mut read = BufReader::new(read);
    verify_daemon_protocol(&mut read, &mut write).await?;
    let handshake = NativeHandshake {
        kind: "horon-agent-native-stdio".into(),
        protocol: PROTOCOL,
        version: horon_agent_build_meta::version().to_string(),
        working_dir: std::env::current_dir()?.to_string_lossy().into_owned(),
        socket_path: socket.to_string_lossy().into_owned(),
    };
    let mut header = serde_json::to_vec(&handshake)?;
    header.push(b'\n');
    if header.len() > HANDSHAKE_LIMIT {
        bail!("native SSH handshake metadata too large");
    }
    output.write_all(&header).await?;
    output.flush().await?;
    {
        let upload = async {
            tokio::io::copy(&mut input, &mut write).await?;
            // A shell pipeline closes stdin after its last request. Forward
            // that half-close but do not discard replies already in flight.
            write.shutdown().await
        };
        let download = tokio::io::copy(&mut read, &mut output);
        tokio::pin!(upload, download);
        tokio::select! {
            result = &mut upload => {
                result?;
                // Detaching a subscribed client must not wait for a remote
                // model turn. Drain final protocol replies for a bounded time.
                if let Ok(result) = tokio::time::timeout(Duration::from_secs(5), &mut download).await {
                    result?;
                }
            },
            result = &mut download => { result?; },
        }
    }
    output.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixListener;

    /// The bridge emits its handshake header before forwarding any native frame.
    async fn read_bridge_header<R: AsyncBufRead + Unpin>(reader: &mut R) {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let header: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(header["kind"].as_str(), Some("horon-agent-native-stdio"));
        assert_eq!(header["protocol"].as_u64(), Some(u64::from(PROTOCOL)));
        assert!(header["socket_path"].as_str().is_some_and(|path| !path.is_empty()));
        assert!(header["working_dir"].as_str().is_some_and(|path| !path.is_empty()));
    }

    #[tokio::test]
    async fn bridge_exchanges_native_frames_and_exits_when_daemon_dies() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (client, bridge) = tokio::io::duplex(4096);
        let (input, output) = tokio::io::split(bridge);
        let task = tokio::spawn(bridge_stream(input, output, socket));
        let (daemon, _) = listener.accept().await.unwrap();
        let (read, mut write) = daemon.into_split();
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        assert_eq!(line, "{\"type\":\"ping\",\"id\":0}\n");
        write
            .write_all(b"{\"type\":\"pong\",\"id\":0,\"native_ssh_protocol\":1}\n")
            .await
            .unwrap();
        let (mut client_read, mut client_write) = tokio::io::split(client);
        let mut client_read = BufReader::new(&mut client_read);
        read_bridge_header(&mut client_read).await;
        client_write.write_all(b"{\"type\":\"ping\",\"id\":1}\n").await.unwrap();
        line.clear();
        read.read_line(&mut line).await.unwrap();
        assert!(line.contains("ping"));
        write.write_all(b"{\"type\":\"pong\",\"id\":1}\n").await.unwrap();
        line.clear();
        client_read.read_line(&mut line).await.unwrap();
        assert!(line.contains("pong"));
        drop(read);
        drop(write);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // client_write deliberately remains open: daemon EOF must still exit.
        drop(client_write);
    }

    #[tokio::test]
    async fn bridge_does_not_claim_handshake_without_daemon() {
        let (client, bridge) = tokio::io::duplex(1024);
        let (input, output) = tokio::io::split(bridge);
        let directory = tempfile::tempdir().unwrap();
        assert!(
            bridge_stream(input, output, directory.path().join("missing.sock"))
                .await
                .is_err()
        );
        let mut client = client;
        let mut output = Vec::new();
        client.read_to_end(&mut output).await.unwrap();
        assert!(output.is_empty());
    }

    #[tokio::test]
    async fn stdio_eof_half_closes_daemon_and_drains_last_reply() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (client, bridge) = tokio::io::duplex(4096);
        let (input, output) = tokio::io::split(bridge);
        let task = tokio::spawn(bridge_stream(input, output, socket));
        let (daemon, _) = listener.accept().await.unwrap();
        let (read, mut write) = daemon.into_split();
        let mut read = BufReader::new(read);
        let mut line = String::new();
        read.read_line(&mut line).await.unwrap();
        write
            .write_all(b"{\"type\":\"pong\",\"id\":0,\"native_ssh_protocol\":1}\n")
            .await
            .unwrap();
        let (read_client, mut write_client) = tokio::io::split(client);
        let mut read_client = BufReader::new(read_client);
        read_bridge_header(&mut read_client).await;
        write_client.write_all(b"{\"type\":\"ping\",\"id\":1}\n").await.unwrap();
        write_client.shutdown().await.unwrap();
        line.clear();
        read.read_line(&mut line).await.unwrap();
        assert!(line.contains("ping"));
        line.clear();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), read.read_line(&mut line))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        // Deliberately reply only after observing the client's write EOF.
        write.write_all(b"{\"type\":\"pong\",\"id\":1}\n").await.unwrap();
        drop(write);
        drop(read);
        read_client.read_to_string(&mut line).await.unwrap();
        assert!(line.contains("pong"));
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn daemon_capability_requires_supported_protocol_and_matching_ping() {
        for frame in [
            "{\"type\":\"pong\",\"id\":0}\n",
            "{\"type\":\"pong\",\"id\":0,\"native_ssh_protocol\":2}\n",
            "{\"type\":\"pong\",\"id\":1,\"native_ssh_protocol\":1}\n",
        ] {
            let mut reader = BufReader::new(frame.as_bytes());
            let mut sink = tokio::io::sink();
            let error = verify_daemon_protocol(&mut reader, &mut sink).await.unwrap_err();
            assert!(error.to_string().contains("update/reload"));
        }
        let frame = b"{\"type\":\"pong\",\"id\":0,\"native_ssh_protocol\":1}\nnative-frame\n";
        let mut reader = BufReader::new(&frame[..]);
        verify_daemon_protocol(&mut reader, &mut tokio::io::sink())
            .await
            .unwrap();
        let mut remaining = String::new();
        reader.read_to_string(&mut remaining).await.unwrap();
        assert_eq!(remaining, "native-frame\n");
    }
}
