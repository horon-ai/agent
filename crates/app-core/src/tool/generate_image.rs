//! `generate_image`: one image call on the `image` role, behind
//! `[tools] generate_image`.
//!
//! This is a thin adapter. Everything that needs the profile, the credential,
//! the endpoint or the storage root is [`horon_agent_base::image_tool`], the same
//! split `aux_complete` uses; the tool owns only the schema, the effects and how
//! a result is worded.
//!
//! Registration is gated twice: the `[tools] generate_image` switch and an
//! `image` entry in the active profile. The second condition is what keeps the
//! role rule honest — a role is added with the workload that calls it, and this
//! tool is that workload — and it also stops a switch flipped before a model was
//! configured from advertising a tool whose every call must fail.

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_base::image_tool::{self, ImageCall};
use horon_agent_tool_types::ToolVerdict;
use serde::Deserialize;
use serde_json::{Value, json};

pub const GENERATE_IMAGE_TOOL: &str = "generate_image";

pub struct GenerateImageTool;

impl GenerateImageTool {
    pub fn new() -> Self {
        Self
    }
}

/// Register the tool on `registry` when [`enabled`].
///
/// Both conditions matter: the switch alone would advertise a tool whose every
/// call must fail, and the role alone would register it for a session that never
/// asked. With either missing the name is neither registered nor in an index
/// row. Registration is idempotent, because the registry is keyed by name.
pub(crate) async fn register(registry: &super::Registry) {
    if !enabled() {
        return;
    }
    registry
        .register(
            GENERATE_IMAGE_TOOL.to_string(),
            std::sync::Arc::new(GenerateImageTool::new()) as std::sync::Arc<dyn Tool>,
        )
        .await;
}

/// Whether the tool should be registered.
pub(crate) fn enabled() -> bool {
    crate::config::config().tools.generate_image && configured()
}

/// Whether the active profile names an `image` model.
pub(crate) fn configured() -> bool {
    active_profile_name().is_some_and(|profile| image_tool::configured_model(&profile).is_some())
}

/// The named provider profile this process runs under, if any.
fn active_profile_name() -> Option<String> {
    [
        "HORON_AGENT_NAMED_PROVIDER_PROFILE",
        "HORON_AGENT_PROVIDER_PROFILE_NAME",
    ]
    .iter()
    .find_map(|key| match std::env::var(key) {
        Ok(value) => Some(value.trim().to_string()).filter(|value| !value.is_empty()),
        Err(_) => None,
    })
}

#[derive(Debug, Deserialize)]
struct GenerateImageInput {
    prompt: String,
    #[serde(default)]
    size: Option<String>,
    #[serde(default)]
    count: Option<u8>,
    #[serde(default)]
    input: Vec<String>,
    #[serde(default)]
    model: Option<String>,
}

impl GenerateImageInput {
    fn into_call(self) -> Result<ImageCall> {
        if self.prompt.len() > image_tool::MAX_PROMPT_BYTES {
            anyhow::bail!(
                "the prompt is {} bytes, above the {}-byte limit",
                self.prompt.len(),
                image_tool::MAX_PROMPT_BYTES
            );
        }
        let call = ImageCall {
            prompt: self.prompt,
            size: self.size.unwrap_or_else(|| "1024x1024".to_string()),
            count: self.count.unwrap_or(1).clamp(1, image_tool::MAX_IMAGES_PER_CALL),
            input: self.input,
            model: self.model,
        };
        call.validate()?;
        Ok(call)
    }
}

#[async_trait]
impl Tool for GenerateImageTool {
    fn name(&self) -> &str {
        GENERATE_IMAGE_TOOL
    }

    fn description(&self) -> &str {
        "Generate an image from a text prompt, or edit existing images, with the profile's image model."
    }

    fn effects(&self) -> ToolEffects {
        // `WRITE` puts the call behind the `write` approval tier and keeps it
        // from running beside another mutating call, because the request and the
        // file are one step. `NETWORK` is the call itself. Together they are why
        // an unattended run does not spend money without approval.
        ToolEffects::write().union(ToolEffects::network())
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["prompt"],
            "properties": {
                "intent": super::intent_schema_property(),
                "prompt": {
                    "type": "string",
                    "description": "What to draw, or what change to make when `input` is given. At most 32 KiB."
                },
                "size": {
                    "type": "string",
                    "enum": image_tool::IMAGE_SIZES,
                    "description": "Output pixel size. Defaults to 1024x1024."
                },
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": image_tool::MAX_IMAGES_PER_CALL,
                    "description": "How many images to return. Defaults to 1."
                },
                "input": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Image paths to edit. Together at most 35 MiB."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model id, overriding the profile's `image` role for this call."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let call = serde_json::from_value::<GenerateImageInput>(input)?.into_call()?;
        let profile = active_profile_name().ok_or_else(|| {
            anyhow::anyhow!(
                "no named provider profile is active, so the `image` role cannot be resolved. \
                 Add `[providers.<name>]` with `roles.image` and select that profile."
            )
        })?;
        let Some(result) = image_tool::generate(&profile, &call).await? else {
            return Ok(ToolOutput::new(format!(
                "Provider profile '{profile}' names no `image` model, so no image was generated \
                     and no request was sent. Add `roles.image = \"<model>\"` to `[providers.{profile}]` \
                     to enable image generation."
            ))
            .with_metadata(json!({ "profile": profile, "count": 0 }))
            .with_verdict(ToolVerdict::Rejected));
        };

        if result.files.is_empty() {
            // A provider that filters a prompt answers with no images and no
            // error. Reporting that as a failure would make the model retry a
            // request that succeeded.
            return Ok(ToolOutput::new(format!(
                "The image endpoint returned no images for the prompt. It reported no error; the \
                 request was accepted and produced nothing. Model: {}.",
                result.model
            ))
            .with_metadata(image_tool::result_metadata(&result))
            .with_verdict(ToolVerdict::Partial));
        }

        let mut text = format!(
            "Generated {} image{} with {} at {}.\n",
            result.files.len(),
            if result.files.len() == 1 { "" } else { "s" },
            result.model,
            result.size
        );
        for file in &result.files {
            text.push_str(&format!(
                "- `{}` ({} bytes, {})\n",
                file.path.display(),
                file.bytes,
                file.mime_type
            ));
        }
        if let Some(revised) = &result.revised_prompt {
            text.push_str(&format!("The provider revised the prompt to: {revised}\n"));
        }
        text.push_str(
            "The images are on disk, not in the conversation. Read one with `read` if its contents \
             need checking.",
        );

        let verdict = if result.is_complete() {
            ToolVerdict::Ok
        } else {
            // Fewer images than asked for is a partial result, not a failure:
            // the caller has usable files.
            ToolVerdict::Partial
        };
        Ok(ToolOutput::new(text)
            .with_title(format!("{} image(s)", result.files.len()))
            .with_metadata(image_tool::result_metadata(&result))
            .with_verdict(verdict))
    }
}

#[cfg(test)]
#[path = "generate_image_tests.rs"]
mod tests;
