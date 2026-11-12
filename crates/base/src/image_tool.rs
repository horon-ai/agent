//! Image generation behind the `image` role.
//!
//! This module owns the work a tool wrapper would otherwise duplicate:
//! resolving the profile's `image` model and endpoint, calling
//! [`horon_agent_provider_openai::images`], and writing the returned bytes to
//! `~/.horon/generated-images/`. The `generate_image` tool in `app-core` is a
//! thin adapter over [`generate`].
//!
//! It lives in `base` rather than `app-core` for the same reason
//! [`crate::aux_completion`] does: the endpoint, the credential and the storage
//! root are all already resolved here, and a tool crate that reached for
//! `provider-openai` and `storage` directly would be a second path to the same
//! three facts.

use anyhow::{Context, Result};
use horon_agent_provider_openai::images::{self, EditRequest, GenerateRequest, ImageInput, MAX_INPUT_IMAGE_BYTES};
use serde_json::json;
use std::path::{Path, PathBuf};

/// Longest one image call may take.
///
/// An image takes far longer than a chat completion and the point of the bound
/// is to fail rather than hang, so the value is generous rather than tight.
pub const IMAGE_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180);

/// A resolved request, before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageCall {
    pub prompt: String,
    pub size: String,
    pub count: u8,
    /// Input image paths for an edit; empty means a generation.
    pub input: Vec<String>,
    /// Model id, or `None` to use the profile's `image` role.
    pub model: Option<String>,
}

impl ImageCall {
    /// Reject a call that cannot succeed before anything is resolved or sent.
    pub fn validate(&self) -> Result<()> {
        if self.prompt.trim().is_empty() {
            anyhow::bail!("the prompt is empty");
        }
        if self.input.len() > MAX_EDIT_INPUTS {
            anyhow::bail!(
                "{} input images were given, above the {MAX_EDIT_INPUTS} an edit accepts",
                self.input.len()
            );
        }
        if !(1..=MAX_IMAGES_PER_CALL).contains(&self.count) {
            anyhow::bail!("the image count {} is outside 1..={MAX_IMAGES_PER_CALL}", self.count);
        }
        Ok(())
    }
}

/// Most input images one edit may carry.
pub const MAX_EDIT_INPUTS: usize = 16;

/// Sizes the endpoint accepts, re-exported so a tool schema need not depend on
/// the transport crate.
pub use horon_agent_provider_openai::images::{IMAGE_SIZES, MAX_IMAGES_PER_CALL};

/// File extension for a MIME type, re-exported for the same reason.
pub use horon_agent_provider_openai::images::extension_for;

impl Default for ImageCall {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            size: "1024x1024".to_string(),
            count: 1,
            input: Vec::new(),
            model: None,
        }
    }
}

/// What one call produced, already written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageResult {
    /// Model the call used.
    pub model: String,
    /// Profile the call went through.
    pub profile: String,
    /// Size requested.
    pub size: String,
    /// Files written, in provider order.
    pub files: Vec<SavedImage>,
    /// The provider's revised prompt for the first image, if it sent one.
    pub revised_prompt: Option<String>,
    /// How many images were asked for, so a caller can tell a partial answer.
    pub requested: u8,
}

impl ImageResult {
    /// Whether every requested image came back.
    pub fn is_complete(&self) -> bool {
        self.files.len() == usize::from(self.requested)
    }
}

/// One image written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedImage {
    pub path: PathBuf,
    pub mime_type: String,
    pub bytes: usize,
}

/// Run one image call.
///
/// `Ok(None)` means the active profile has no `image` model, which the tool
/// reports as "not configured" rather than as a failed request. An `image` model
/// with a broken profile is `Err`, because that is a mistake worth reporting.
pub async fn generate(profile_name: &str, call: &ImageCall) -> Result<Option<ImageResult>> {
    call.validate()?;
    let Some(endpoint) = crate::provider::external::image_endpoint(profile_name)? else {
        return Ok(None);
    };
    let model = match call.model.as_deref().map(str::trim).filter(|model| !model.is_empty()) {
        Some(model) => model.to_string(),
        None => crate::provider::external::image_model(profile_name)
            .ok_or_else(|| anyhow::anyhow!("Provider profile '{profile_name}' names no `image` model."))?,
    };

    let client = reqwest::Client::builder()
        .timeout(IMAGE_CALL_TIMEOUT)
        .build()
        .map_err(|error| anyhow::anyhow!("the image HTTP client could not be built: {error}"))?;

    let reply = if call.input.is_empty() {
        let request = GenerateRequest {
            model: model.clone(),
            prompt: call.prompt.clone(),
            size: call.size.clone(),
            count: call.count,
            output_format: None,
        };
        images::generate(
            &client,
            &endpoint.base_url,
            &endpoint.auth,
            endpoint.api_key.as_deref(),
            endpoint.extra_body.as_ref(),
            &request,
        )
        .await?
    } else {
        let inputs = load_inputs(&call.input)?;
        let request = EditRequest {
            model: model.clone(),
            prompt: call.prompt.clone(),
            size: call.size.clone(),
            count: call.count,
            inputs,
        };
        images::edit(
            &client,
            &endpoint.base_url,
            &endpoint.auth,
            endpoint.api_key.as_deref(),
            endpoint.extra_body.as_ref(),
            &request,
        )
        .await?
    };

    if reply.images.is_empty() {
        return Ok(Some(ImageResult {
            model,
            profile: endpoint.profile,
            size: call.size.clone(),
            files: Vec::new(),
            revised_prompt: None,
            requested: call.count,
        }));
    }

    let directory = image_directory()?;
    std::fs::create_dir_all(&directory).with_context(|| {
        format!(
            "the generated-image directory {} could not be created",
            directory.display()
        )
    })?;
    let mut files = Vec::with_capacity(reply.images.len());
    let mut revised_prompt = None;
    for image in &reply.images {
        let path = save_image(&directory, &image.bytes, &image.mime_type)?;
        if revised_prompt.is_none() {
            revised_prompt = image.revised_prompt.clone();
        }
        files.push(SavedImage {
            path,
            mime_type: image.mime_type.clone(),
            bytes: image.bytes.len(),
        });
    }
    Ok(Some(ImageResult {
        model,
        profile: endpoint.profile,
        size: call.size.clone(),
        files,
        revised_prompt,
        requested: call.count,
    }))
}

/// The metadata a tool result carries, so a reader does not parse prose.
pub fn result_metadata(result: &ImageResult) -> serde_json::Value {
    json!({
        "model": result.model,
        "profile": result.profile,
        "size": result.size,
        "count": result.files.len(),
        "requested": result.requested,
        "images": result.files.iter().map(|file| json!({
            "path": file.path.display().to_string(),
            "mime_type": file.mime_type,
            "bytes": file.bytes,
        })).collect::<Vec<_>>(),
    })
}

/// `~/.horon/generated-images/`, the one place generated images land.
fn image_directory() -> Result<PathBuf> {
    Ok(horon_agent_storage::horon_agent_dir()?.join("generated-images"))
}

/// Write one image, returning its path.
///
/// The name carries a millisecond stamp plus a counter, and no provider-supplied
/// text reaches it: an id from a server is untrusted and is not a file name. An
/// existing file is never overwritten, because the user may already be looking
/// at it.
fn save_image(directory: &Path, bytes: &[u8], mime_type: &str) -> Result<PathBuf> {
    let extension = images::extension_for(mime_type);
    let stamp = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => duration.as_millis(),
        // A clock before the epoch is not a reason to fail a write; the counter
        // suffix still keeps the name unique.
        Err(_) => 0,
    };
    for attempt in 0..1000u32 {
        let name = if attempt == 0 {
            format!("image-{stamp}.{extension}")
        } else {
            format!("image-{stamp}-{attempt}.{extension}")
        };
        let path = directory.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                use std::io::Write as _;
                file.write_all(bytes)
                    .with_context(|| format!("the image {} could not be written", path.display()))?;
                return Ok(path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                anyhow::bail!("the image {} could not be created: {error}", path.display());
            }
        }
    }
    anyhow::bail!("no unused image name was found in {}", directory.display())
}

/// Read the input images for an edit.
fn load_inputs(paths: &[String]) -> Result<Vec<ImageInput>> {
    let mut inputs = Vec::with_capacity(paths.len());
    let mut total = 0usize;
    for path in paths {
        let path = PathBuf::from(path);
        let bytes =
            std::fs::read(&path).with_context(|| format!("the input image {} could not be read", path.display()))?;
        if bytes.is_empty() {
            anyhow::bail!("the input image {} is empty", path.display());
        }
        total += bytes.len();
        if total > MAX_INPUT_IMAGE_BYTES {
            anyhow::bail!("the input images together exceed the {MAX_INPUT_IMAGE_BYTES}-byte limit");
        }
        // An unrecognised file is sent as PNG because that is what
        // OpenAI-compatible edit endpoints require; the endpoint rejects a
        // non-image with a message better than a local guess would give.
        let mime_type = images::sniff_mime_type(&bytes).unwrap_or("image/png").to_string();
        let file_name = match path.file_name() {
            Some(name) => name.to_string_lossy().to_string(),
            None => "input.png".to_string(),
        };
        inputs.push(ImageInput {
            file_name,
            mime_type,
            bytes,
        });
    }
    Ok(inputs)
}

/// The `image` role this profile names, for a caller that only wants to know
/// whether the feature is configured.
pub fn configured_model(profile_name: &str) -> Option<String> {
    crate::provider::external::image_model(profile_name)
}

/// Largest accepted prompt, matching the tool's schema description.
pub const MAX_PROMPT_BYTES: usize = 32 * 1024;

#[cfg(test)]
#[path = "image_tool_tests.rs"]
mod tests;
