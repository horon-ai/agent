//! The final outbound projection of one provider request.
//!
//! Two steps decide what the provider receives, and neither touches the stored
//! transcript: secret masking replaces credentials with the vault's
//! placeholders, and the `pre_request` hook may rewrite the message list for
//! this request only.
//!
//! Both steps are skipped when they are not needed, and the caller keeps
//! borrowing the turn's own snapshot: with `secrets` off and no hook configured
//! this function returns `None` without copying a message or spawning a
//! process.

use horon_agent_message_types::Message;

/// Project `messages` for one provider request.
///
/// Takes the request's facts instead of the agent so the turn loop can call it
/// without holding a borrow of the whole agent across the hook's await.
///
/// Returns the replacement list, or `None` when the request may send the
/// messages it was built from unchanged.
pub(super) async fn project(
    context: &crate::hooks::pre_request::RequestContext<'_>,
    messages: &[Message],
) -> anyhow::Result<Option<Vec<Message>>> {
    // The provider reads placeholders; the vault keeps the real bytes for the
    // tool that later needs them. `block` refuses the request here.
    let mode = crate::config::config().tools.secrets_mode;
    let masked = crate::secrets::outbound_messages(messages, context.session_id, mode)?;
    if masked.is_some() {
        record_request_extra_clone(messages);
    }
    let base = masked.as_deref().unwrap_or(messages);
    // The hook sees the masked text and nothing raw, and an unset hook returns
    // before the payload is built.
    let rewritten = crate::hooks::pre_request::rewrite(context, base).await;
    if rewritten.is_some() {
        record_request_extra_clone(base);
    }
    Ok(rewritten.or(masked))
}

/// Append an owned suffix to a borrowed transcript prefix.
///
/// Called only when a request actually grows beyond its snapshot (memory
/// injection, plan-limit reminder, batch nudge). An unmodified request keeps
/// borrowing the snapshot and never reaches this.
pub(crate) fn join_request_messages(prefix: &[Message], suffix: &[Message]) -> Vec<Message> {
    let mut joined = Vec::with_capacity(prefix.len() + suffix.len());
    joined.extend_from_slice(prefix);
    joined.extend(suffix.iter().cloned());
    joined
}

/// Full-transcript copies made beyond the turn's one owned snapshot; test-only.
#[cfg(test)]
static REQUEST_EXTRA_CLONES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
static REQUEST_EXTRA_CLONE_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
pub(crate) fn request_extra_clone_counters() -> (u64, u64) {
    (
        REQUEST_EXTRA_CLONES.load(std::sync::atomic::Ordering::Relaxed),
        REQUEST_EXTRA_CLONE_BYTES.load(std::sync::atomic::Ordering::Relaxed),
    )
}

#[cfg(test)]
pub(crate) fn record_request_extra_clone(copied: &[Message]) {
    REQUEST_EXTRA_CLONES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let bytes = u64::try_from(super::stable_json_len(copied)).unwrap_or(u64::MAX);
    REQUEST_EXTRA_CLONE_BYTES.fetch_add(bytes, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(not(test))]
#[inline]
pub(crate) fn record_request_extra_clone(_copied: &[Message]) {}

#[cfg(test)]
#[path = "request_messages_tests.rs"]
mod tests;
