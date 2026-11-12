//! Image generation and editing on the OpenAI Images shape.
//!
//! This is the one documented exception to "one model path": a chat turn posts
//! `/chat/completions`, and this module posts `/images/generations` or
//! `/images/edits` on the same OpenAI-compatible profile, with the same base
//! URL, credential, headers and `extra_body`. There is still no second provider
//! runtime, no router and no plugin host, so the amendment is a route, not an
//! architecture.
//!
//! The module deliberately does no file I/O. It answers with the decoded bytes
//! and the provider's own metadata, and the caller decides where an image lands;
//! that keeps this crate free of a storage policy and lets the tool decide how an
//! artifact is named.

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::chat::transport::{self, ChatAuth};

/// Path appended to the profile's endpoint root for a new image.
pub const IMAGE_GENERATIONS_PATH: &str = "/images/generations";

/// Path appended to the profile's endpoint root for an edit.
pub const IMAGE_EDITS_PATH: &str = "/images/edits";

/// Sizes an OpenAI Images endpoint accepts.
pub const IMAGE_SIZES: [&str; 3] = ["1024x1024", "1536x1024", "1024x1536"];

/// Most images one call may ask for.
pub const MAX_IMAGES_PER_CALL: u8 = 4;

/// Largest accepted input image for an edit.
pub const MAX_INPUT_IMAGE_BYTES: usize = 35 * 1024 * 1024;

/// One image the provider returned, already decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    /// Decoded image bytes.
    pub bytes: Vec<u8>,
    /// MIME type the provider declared, or the one implied by the request.
    pub mime_type: String,
    /// Provider's rewritten prompt, when it returns one.
    pub revised_prompt: Option<String>,
}

/// What one image call produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageReply {
    /// Images the provider returned, in order. Empty is a valid answer.
    pub images: Vec<GeneratedImage>,
    /// Model the endpoint reported, if it reported one.
    pub model: Option<String>,
}

/// A request for a new image.
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    pub model: String,
    pub prompt: String,
    /// One of [`IMAGE_SIZES`].
    pub size: String,
    /// 1 to [`MAX_IMAGES_PER_CALL`].
    pub count: u8,
    /// Requested output format (`png`, `jpeg`, `webp`), when the endpoint
    /// supports choosing one.
    pub output_format: Option<String>,
}

/// A request that edits an existing image.
#[derive(Debug, Clone)]
pub struct EditRequest {
    pub model: String,
    pub prompt: String,
    pub size: String,
    pub count: u8,
    /// Input images: file name, MIME type and bytes.
    pub inputs: Vec<ImageInput>,
}

/// One input image for an edit.
#[derive(Debug, Clone)]
pub struct ImageInput {
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
}

fn join_path(base_url: &str, path: &str) -> String {
    format!("{}{path}", base_url.trim_end_matches('/'))
}

/// Send one generation request.
pub async fn generate(
    client: &reqwest::Client,
    base_url: &str,
    auth: &ChatAuth,
    api_key: Option<&str>,
    extra_body: Option<&Value>,
    request: &GenerateRequest,
) -> Result<ImageReply> {
    validate_size(&request.size)?;
    validate_count(request.count)?;
    if request.prompt.trim().is_empty() {
        anyhow::bail!("the image prompt is empty");
    }

    let mut body = serde_json::json!({
        "model": request.model,
        "prompt": request.prompt,
        "n": request.count,
        "size": request.size,
    });
    if let Some(format) = &request.output_format {
        body["output_format"] = Value::String(format.clone());
    }
    merge_extra_body(&mut body, extra_body);

    let endpoint = join_path(base_url, IMAGE_GENERATIONS_PATH);
    let builder = client.post(&endpoint).json(&body);
    let builder = transport::apply_auth(builder, auth, api_key)?;
    let response = builder
        .send()
        .await
        .with_context(|| format!("the image request to {endpoint} could not be sent"))?;
    decode_reply(response, &endpoint, api_key).await
}

/// Send one edit request as multipart, which is the only shape OpenAI-compatible
/// endpoints accept for `/images/edits`.
pub async fn edit(
    client: &reqwest::Client,
    base_url: &str,
    auth: &ChatAuth,
    api_key: Option<&str>,
    extra_body: Option<&Value>,
    request: &EditRequest,
) -> Result<ImageReply> {
    validate_size(&request.size)?;
    validate_count(request.count)?;
    if request.prompt.trim().is_empty() {
        anyhow::bail!("the image prompt is empty");
    }
    if request.inputs.is_empty() {
        anyhow::bail!("an edit needs at least one input image");
    }
    let mut total = 0usize;
    for input in &request.inputs {
        if input.bytes.is_empty() {
            anyhow::bail!("the input image {} is empty", input.file_name);
        }
        if input.bytes.len() > MAX_INPUT_IMAGE_BYTES {
            anyhow::bail!(
                "the input image {} is {} bytes, above the {MAX_INPUT_IMAGE_BYTES}-byte limit",
                input.file_name,
                input.bytes.len()
            );
        }
        total += input.bytes.len();
        if total > MAX_INPUT_IMAGE_BYTES {
            anyhow::bail!("the input images exceed the {MAX_INPUT_IMAGE_BYTES}-byte limit together");
        }
    }

    let mut form = reqwest::multipart::Form::new()
        .text("model", request.model.clone())
        .text("prompt", request.prompt.clone())
        .text("n", request.count.to_string())
        .text("size", request.size.clone());
    for input in &request.inputs {
        let part = reqwest::multipart::Part::bytes(input.bytes.clone())
            .file_name(input.file_name.clone())
            .mime_str(&input.mime_type)
            .with_context(|| format!("the input image {} has an unusable MIME type", input.file_name))?;
        form = form.part("image[]", part);
    }
    // `extra_body` rides as one JSON field because a multipart form has no
    // body object to merge into. Endpoints that do not read it ignore it.
    if let Some(extra) = extra_body {
        form = form.text("extra_body", serde_json::to_string(extra)?);
    }

    let endpoint = join_path(base_url, IMAGE_EDITS_PATH);
    let builder = client.post(&endpoint).multipart(form);
    let builder = transport::apply_auth(builder, auth, api_key)?;
    let response = builder
        .send()
        .await
        .with_context(|| format!("the image edit request to {endpoint} could not be sent"))?;
    decode_reply(response, &endpoint, api_key).await
}

fn validate_size(size: &str) -> Result<()> {
    if IMAGE_SIZES.contains(&size) {
        return Ok(());
    }
    anyhow::bail!("the image size {size} is not one of {}", IMAGE_SIZES.join(", "))
}

fn validate_count(count: u8) -> Result<()> {
    if (1..=MAX_IMAGES_PER_CALL).contains(&count) {
        return Ok(());
    }
    anyhow::bail!("the image count {count} is outside 1..={MAX_IMAGES_PER_CALL}")
}

/// Profile-level fields override the generated ones, the same rule the chat
/// request follows, so a gateway that needs an extra field can add it without a
/// code change.
fn merge_extra_body(body: &mut Value, extra_body: Option<&Value>) {
    let (Some(body), Some(extra)) = (body.as_object_mut(), extra_body.and_then(Value::as_object)) else {
        return;
    };
    for (key, value) in extra {
        body.insert(key.clone(), value.clone());
    }
}

/// The OpenAI Images response shape, kept private because it is a wire detail.
#[derive(Debug, Deserialize)]
struct WireReply {
    #[serde(default)]
    data: Vec<WireImage>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireImage {
    #[serde(default)]
    b64_json: Option<String>,
    #[serde(default)]
    revised_prompt: Option<String>,
}

async fn decode_reply(response: reqwest::Response, endpoint: &str, api_key: Option<&str>) -> Result<ImageReply> {
    let status = response.status();
    if !status.is_success() {
        let retry_after = horon_agent_provider_core::retry_after::retry_after(response.headers());
        // The body is context for the status; a body that cannot be read leaves
        // the status and the retry hint intact.
        let body = response
            .text()
            .await
            .unwrap_or_else(|error| format!("<body unreadable: {error}>"));
        return Err(transport::http_response_error(
            endpoint,
            status.as_u16(),
            retry_after,
            &body,
            api_key,
        ));
    }
    let body = response
        .text()
        .await
        .with_context(|| format!("the image response from {endpoint} could not be read"))?;
    parse_reply(&body)
}

/// Parse an Images response body.
///
/// A `200` with no `data` entries is a zero-image answer and not an error: an
/// endpoint that filters a prompt returns an empty list, and the caller reports
/// "no images" rather than a failure. A `b64_json` that does not decode is an
/// error, because silently dropping it would turn a corrupt response into a
/// successful empty one.
pub fn parse_reply(body: &str) -> Result<ImageReply> {
    let wire: WireReply = serde_json::from_str(body).context("the image response was not valid JSON")?;
    let mut images = Vec::with_capacity(wire.data.len());
    for (index, image) in wire.data.into_iter().enumerate() {
        let Some(encoded) = image.b64_json else {
            // A URL-only entry is not in scope: this module never fetches a
            // second host, and the tool reports what it received.
            anyhow::bail!("the image entry {index} carried no base64 payload");
        };
        let bytes = BASE64_STANDARD
            .decode(encoded.as_bytes())
            .with_context(|| format!("the image entry {index} was not valid base64"))?;
        let mime_type = match sniff_mime_type(&bytes) {
            Some(sniffed) => sniffed.to_string(),
            None => "image/png".to_string(),
        };
        images.push(GeneratedImage {
            bytes,
            mime_type,
            revised_prompt: image.revised_prompt,
        });
    }
    Ok(ImageReply {
        images,
        model: wire.model,
    })
}

/// Identify an image by its magic bytes.
///
/// The provider's declared format is not always present, and a file extension
/// derived from a guess would be wrong more often than a sniff is.
pub fn sniff_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("image/gif");
    }
    None
}

/// File extension for a MIME type this module recognises.
pub fn extension_for(mime_type: &str) -> &'static str {
    match mime_type {
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        "image/gif" => "gif",
        _ => "png",
    }
}

/// The request body a generation sends, exposed so a test can assert the wire
/// shape without a server.
#[derive(Debug, Serialize)]
pub struct GenerateWire<'a> {
    pub model: &'a str,
    pub prompt: &'a str,
    pub n: u8,
    pub size: &'a str,
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
