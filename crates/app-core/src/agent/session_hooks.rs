//! Session lifecycle observer hooks, kept out of the already-large agent module.
//!
//! The method fires `session_start`/`session_end` and records the firing for the
//! per-session observation view. Recording is a no-op unless that view was
//! opted into, and it never touches the conversation.

use crate::agent::Agent;

impl Agent {
    /// Fire a session lifecycle observer hook (`session_start`/`session_end`).
    /// No-op when the hook is not configured.
    pub(crate) fn fire_session_lifecycle_hook(&self, event_name: &'static str, source: &str) {
        if !crate::hooks::hook_configured(event_name) {
            return;
        }
        crate::server::session_notes::note_hook(self.session_id(), event_name, Some(source));
        let mut event = crate::hooks::HookEvent::new(event_name)
            .session_id(self.session.id.clone())
            .field("SOURCE", source)
            .field("MODEL", self.provider_model());
        if let Some(cwd) = self.working_dir() {
            event = event.cwd(cwd);
        }
        crate::hooks::dispatch_observer(event);
    }
}
