//! Attachments a client sends with a user message or a soft interrupt, turned
//! into the content blocks the transcript stores.
//!
//! An attachment is a `(mime, base64 data)` pair on the wire. `audio/wav` and
//! `audio/mpeg` are audio input, so they become [`ContentBlock::Audio`] with the
//! container name the providers expect, which is what makes a provider request
//! carry `input_audio`. Every other mime keeps the meaning it always had in this
//! position and becomes [`ContentBlock::Image`], because an editor that already
//! sends attachments sends images.

use crate::message::ContentBlock;

/// The provider `format` name for an audio mime type, if this is audio at all.
///
/// Only the two container names the providers accept appear here; any other
/// mime stays an image attachment exactly as before.
fn audio_format(media_type: &str) -> Option<&'static str> {
    match media_type {
        "audio/wav" => Some("wav"),
        "audio/mpeg" => Some("mp3"),
        _ => None,
    }
}

/// Content blocks for one attachment list, in the order it was sent.
pub(in crate::agent) fn attachment_blocks(attachments: Vec<(String, String)>) -> Vec<ContentBlock> {
    attachments
        .into_iter()
        .map(|(media_type, data)| match audio_format(&media_type) {
            Some(format) => ContentBlock::Audio {
                format: format.to_string(),
                data,
            },
            None => ContentBlock::Image { media_type, data },
        })
        .collect()
}

#[cfg(test)]
#[path = "audio_tests.rs"]
mod audio_tests;
