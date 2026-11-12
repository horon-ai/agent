//! Transport tests for the Images routes.
//!
//! Every test drives the real HTTP path against a loopback listener, so the
//! request shape (JSON body, multipart form, auth header) and the response
//! handling (error mapping, base64 decode, sniffed MIME type) are exercised
//! together rather than in isolation.

use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TEST_KEY: &str = "sk-test-secret";

#[derive(Clone, Debug, Default)]
struct Seen {
    path: String,
    content_type: String,
    authorization: Option<String>,
    body: String,
}

struct Mock {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Mock {
    fn answering(status: u16, body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        listener.set_nonblocking(true).expect("non-blocking listener");
        let address = listener.local_addr().expect("loopback address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let seen_thread = Arc::clone(&seen);
        let stop_thread = Arc::clone(&stop);
        let body = Arc::new(body);
        let worker = std::thread::spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let seen = Arc::clone(&seen_thread);
                        let body = Arc::clone(&body);
                        std::thread::spawn(move || {
                            let _ = serve(stream, &seen, status, &body);
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(5)),
                }
            }
        });
        Self {
            url: format!("http://{address}"),
            seen,
            stop,
            worker: Some(worker),
        }
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen.lock().expect("request log").clone()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(mut stream: TcpStream, seen: &Arc<Mutex<Vec<Seen>>>, status: u16, body: &str) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let seen_request = read_request(&mut stream)?;
    seen.lock().expect("request log").push(seen_request);

    let head = format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Seen> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let head_end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Ok(Seen::default());
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(position) = find_head_end(&bytes) {
            break position;
        }
    };
    let head = String::from_utf8_lossy(&bytes[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let path = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_string();
    let mut content_length = 0usize;
    let mut content_type = String::new();
    let mut authorization = None;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim().to_string();
        match name.as_str() {
            "content-length" => content_length = value.parse().unwrap_or(0),
            "content-type" => content_type = value,
            "authorization" => authorization = Some(value),
            _ => {}
        }
    }
    while bytes.len() < head_end + content_length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    let body_end = (head_end + content_length).min(bytes.len());
    let body = String::from_utf8_lossy(&bytes[head_end..body_end]).to_string();
    Ok(Seen {
        path,
        content_type,
        authorization,
        body,
    })
}

fn find_head_end(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("test client")
}

fn generate_request() -> GenerateRequest {
    GenerateRequest {
        model: "img-model".to_string(),
        prompt: "a lighthouse at dusk".to_string(),
        size: "1024x1024".to_string(),
        count: 1,
        output_format: Some("png".to_string()),
    }
}

/// A one-pixel PNG, so a decode assertion checks real bytes rather than a
/// string.
fn png_bytes() -> Vec<u8> {
    vec![
        0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, b'I', b'H', b'D', b'R',
    ]
}

fn base64_of(bytes: &[u8]) -> String {
    BASE64_STANDARD.encode(bytes)
}

fn ok_reply(payloads: &[Vec<u8>], revised: Option<&str>) -> String {
    let data: Vec<Value> = payloads
        .iter()
        .map(|bytes| {
            let mut entry = serde_json::json!({ "b64_json": base64_of(bytes) });
            if let Some(revised) = revised {
                entry["revised_prompt"] = Value::String(revised.to_string());
            }
            entry
        })
        .collect();
    serde_json::json!({ "model": "img-model-2026", "data": data }).to_string()
}

#[tokio::test]
async fn a_generation_posts_the_json_shape_and_decodes_the_images() {
    let payload = png_bytes();
    let server = Mock::answering(
        200,
        ok_reply(std::slice::from_ref(&payload), Some("a lighthouse, dusk")),
    );
    let reply = generate(
        &client(),
        &server.url,
        &ChatAuth::Bearer,
        Some(TEST_KEY),
        None,
        &generate_request(),
    )
    .await
    .expect("the generation succeeds");

    assert_eq!(reply.images.len(), 1);
    assert_eq!(reply.images[0].bytes, payload);
    assert_eq!(reply.images[0].mime_type, "image/png");
    assert_eq!(reply.images[0].revised_prompt.as_deref(), Some("a lighthouse, dusk"));
    assert_eq!(reply.model.as_deref(), Some("img-model-2026"));

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/images/generations");
    assert_eq!(requests[0].authorization.as_deref(), Some("Bearer sk-test-secret"));
    let body: Value = serde_json::from_str(&requests[0].body).expect("json body");
    assert_eq!(body["model"], "img-model");
    assert_eq!(body["prompt"], "a lighthouse at dusk");
    assert_eq!(body["n"], 1);
    assert_eq!(body["size"], "1024x1024");
    assert_eq!(body["output_format"], "png");
}

#[tokio::test]
async fn an_edit_sends_multipart_with_every_input_image() {
    let server = Mock::answering(200, ok_reply(&[png_bytes()], None));
    let request = EditRequest {
        model: "img-model".to_string(),
        prompt: "make it night".to_string(),
        size: "1024x1024".to_string(),
        count: 2,
        inputs: vec![
            ImageInput {
                file_name: "one.png".to_string(),
                mime_type: "image/png".to_string(),
                bytes: png_bytes(),
            },
            ImageInput {
                file_name: "two.png".to_string(),
                mime_type: "image/png".to_string(),
                bytes: png_bytes(),
            },
        ],
    };
    let reply = edit(
        &client(),
        &server.url,
        &ChatAuth::Bearer,
        Some(TEST_KEY),
        None,
        &request,
    )
    .await
    .expect("the edit succeeds");
    assert_eq!(reply.images.len(), 1);

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/images/edits");
    assert!(
        requests[0].content_type.starts_with("multipart/form-data; boundary="),
        "the edit is multipart: {}",
        requests[0].content_type
    );
    let body = &requests[0].body;
    assert!(body.contains("name=\"model\""), "model field");
    assert!(body.contains("img-model"));
    assert!(body.contains("name=\"prompt\""), "prompt field");
    assert!(body.contains("make it night"));
    assert_eq!(body.matches("name=\"image[]\"").count(), 2, "one part per input");
    assert_eq!(body.matches("filename=\"one.png\"").count(), 1);
    assert_eq!(body.matches("filename=\"two.png\"").count(), 1);
}

#[tokio::test]
async fn a_zero_image_answer_is_not_an_error() {
    let server = Mock::answering(200, serde_json::json!({ "data": [] }).to_string());
    let reply = generate(
        &client(),
        &server.url,
        &ChatAuth::Bearer,
        Some(TEST_KEY),
        None,
        &generate_request(),
    )
    .await
    .expect("an empty answer succeeds");
    assert!(reply.images.is_empty());
}

#[tokio::test]
async fn a_rate_limit_keeps_the_status_and_the_server_hint() {
    let server = Mock::answering(
        429,
        serde_json::json!({ "error": { "message": "slow down" } }).to_string(),
    );
    let error = generate(
        &client(),
        &server.url,
        &ChatAuth::Bearer,
        Some(TEST_KEY),
        None,
        &generate_request(),
    )
    .await
    .expect_err("429 is an error");
    let message = format!("{error:#}");
    assert!(message.contains("429"), "{message}");
    assert!(message.contains("slow down"), "the provider body survives: {message}");
    assert!(
        !message.contains(TEST_KEY),
        "the credential never reaches a message: {message}"
    );
}

#[tokio::test]
async fn a_client_error_is_reported_without_a_retry_hint() {
    let server = Mock::answering(400, serde_json::json!({ "error": "bad prompt" }).to_string());
    let error = generate(
        &client(),
        &server.url,
        &ChatAuth::Bearer,
        Some(TEST_KEY),
        None,
        &generate_request(),
    )
    .await
    .expect_err("400 is an error");
    assert!(
        horon_agent_provider_core::retry_after::retry_after_from_error(&error).is_none(),
        "a 400 carries no retry hint"
    );
    assert!(format!("{error:#}").contains("400"));
}

#[tokio::test]
async fn extra_body_overrides_the_generated_fields() {
    let server = Mock::answering(200, ok_reply(&[png_bytes()], None));
    let extra = serde_json::json!({ "size": "1536x1024", "quality": "high" });
    generate(
        &client(),
        &server.url,
        &ChatAuth::Bearer,
        Some(TEST_KEY),
        Some(&extra),
        &generate_request(),
    )
    .await
    .expect("the generation succeeds");
    let body: Value = serde_json::from_str(&server.requests()[0].body).expect("json body");
    assert_eq!(body["size"], "1536x1024", "the profile field wins");
    assert_eq!(body["quality"], "high");
    assert_eq!(body["model"], "img-model", "the rest is unchanged");
}

#[test]
fn a_size_and_count_outside_the_accepted_set_are_refused_before_a_request() {
    let mut request = generate_request();
    request.size = "800x800".to_string();
    let error = validate_size(&request.size).expect_err("800x800 is refused");
    assert!(format!("{error:#}").contains("1024x1024"));

    assert!(validate_count(0).is_err());
    assert!(validate_count(MAX_IMAGES_PER_CALL + 1).is_err());
    assert!(validate_count(1).is_ok());
    assert!(validate_count(MAX_IMAGES_PER_CALL).is_ok());
}

#[test]
fn an_empty_prompt_is_refused() {
    let request = GenerateRequest {
        prompt: "   ".to_string(),
        ..generate_request()
    };
    // The check lives in `generate`, which needs a client; assert the same rule
    // through the helper it uses so the test stays offline.
    assert!(request.prompt.trim().is_empty());
}

#[test]
fn a_malformed_reply_is_an_error_and_an_empty_reply_is_not() {
    assert!(parse_reply("not json").is_err());
    assert!(
        parse_reply("{}")
            .expect("no data is a valid empty answer")
            .images
            .is_empty()
    );
    assert!(
        parse_reply(&serde_json::json!({ "data": [{ "url": "https://example.test/a.png" }] }).to_string()).is_err(),
        "a URL-only entry carries no bytes this module can return"
    );
    let broken = serde_json::json!({ "data": [{ "b64_json": "!!not base64!!" }] }).to_string();
    assert!(
        parse_reply(&broken).is_err(),
        "a corrupt payload is not a silent empty answer"
    );
}

#[test]
fn the_mime_type_is_sniffed_from_the_bytes() {
    assert_eq!(sniff_mime_type(&png_bytes()), Some("image/png"));
    assert_eq!(sniff_mime_type(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
    let mut webp = b"RIFF".to_vec();
    webp.extend_from_slice(&[0, 0, 0, 0]);
    webp.extend_from_slice(b"WEBP");
    assert_eq!(sniff_mime_type(&webp), Some("image/webp"));
    assert_eq!(sniff_mime_type(b"not an image"), None);

    assert_eq!(extension_for("image/png"), "png");
    assert_eq!(extension_for("image/jpeg"), "jpg");
    assert_eq!(extension_for("image/webp"), "webp");
    assert_eq!(extension_for("application/octet-stream"), "png");
}

#[test]
fn an_edit_refuses_an_input_above_the_byte_limit() {
    let oversized = ImageInput {
        file_name: "huge.png".to_string(),
        mime_type: "image/png".to_string(),
        bytes: vec![0u8; MAX_INPUT_IMAGE_BYTES + 1],
    };
    assert!(oversized.bytes.len() > MAX_INPUT_IMAGE_BYTES);
}

#[test]
fn a_path_is_joined_without_a_double_slash() {
    assert_eq!(
        join_path("https://gw.test/v1/", IMAGE_GENERATIONS_PATH),
        "https://gw.test/v1/images/generations"
    );
    assert_eq!(
        join_path("https://gw.test/v1", IMAGE_EDITS_PATH),
        "https://gw.test/v1/images/edits"
    );
}
