//! Prompt-safe rendering of an inbound message.

use crate::message::IrcMessage;

/// Render one message as an `<irc>` element the model can read and reply to.
pub fn format_envelope(msg: &IrcMessage, from_handle: Option<&str>) -> String {
    let mut out = String::with_capacity(msg.body.len() + 96);
    out.push_str("<irc id=\"");
    write_attribute(&mut out, &msg.id.to_string());
    out.push_str("\" from=\"");
    write_attribute(&mut out, &msg.from);
    out.push('"');
    if let Some(handle) = from_handle
        && !handle.is_empty()
    {
        out.push_str(" handle=\"");
        write_attribute(&mut out, handle);
        out.push('"');
    }
    if let Some(reply_to) = msg.reply_to {
        out.push_str(" reply_to=\"");
        write_attribute(&mut out, &reply_to.to_string());
        out.push('"');
    }
    if msg.progress {
        out.push_str(" progress=\"true\"");
    }
    if msg.broadcast {
        out.push_str(" broadcast=\"true\"");
    }
    if msg.wake_relay {
        out.push_str(" wake_relay=\"true\"");
    }
    out.push('>');
    write_body(&mut out, &msg.body);
    out.push_str("</irc>");
    out
}

fn write_attribute(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '&' => out.push_str("&amp;"),
            other => out.push(other),
        }
    }
}

/// Neutralize body tags that would forge prompt structure, leaving other text intact.
fn write_body(out: &mut String, body: &str) {
    const TAGS: [&str; 4] = ["<irc", "</irc", "<system-reminder", "</system-reminder"];
    let mut rest = body;
    while !rest.is_empty() {
        let next = TAGS
            .iter()
            .filter_map(|tag| rest.find(tag).map(|index| (index, *tag)))
            .min_by_key(|(index, _)| *index);
        let Some((index, tag)) = next else {
            out.push_str(rest);
            return;
        };
        out.push_str(&rest[..index]);
        out.push_str("&lt;");
        out.push_str(&tag[1..]);
        rest = &rest[index + tag.len()..];
    }
}
