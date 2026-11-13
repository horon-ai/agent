//! Waiting for a notification the agent writes as a consequence of a client
//! request, kept apart from `acp_client.rs` so the trampoline file stays inside
//! its size budget.

use super::*;

impl AcpClient {
    /// The next buffered notification, or the next one the agent writes.
    pub fn next_notification(&mut self, deadline: Duration) -> Result<Value, AcpClientError> {
        if let Some(notification) = self.notifications.pop_front() {
            return Ok(notification);
        }
        let end = Instant::now() + deadline;
        match self.next_frame(end)? {
            Frame::Notification { method, params } => Ok(json!({ "method": method, "params": params })),
            other => Err(AcpClientError::Io(format!("expected a notification, saw {other:?}"))),
        }
    }

    /// Every notification buffered so far.
    pub fn drain_notifications(&mut self) -> Vec<Value> {
        self.notifications.drain(..).collect()
    }

    /// Wait for the `$/cancel_request` the agent sends for `request_id`,
    /// keeping every other notification for [`Self::drain_notifications`].
    ///
    /// The adapter writes the cancel only after it has sent the daemon
    /// `Cancel`, so the notification can arrive *after* the prompt's own
    /// `cancelled` response. Reading the buffer right after that response
    /// therefore races the adapter under load: the same test passed and failed
    /// across runs of one commit (runs 43451256902 and 43461968074). Waiting
    /// for the notification makes the assertion about the agent's behaviour
    /// instead of about the order two writes reached the client.
    pub fn await_cancel_request(&mut self, request_id: &Value, deadline: Duration) -> Result<Value, AcpClientError> {
        let matches =
            |note: &Value| note["method"] == "$/cancel_request" && same_id(&note["params"]["requestId"], request_id);
        if let Some(position) = self.notifications.iter().position(matches) {
            // The index came from this deque and nothing removed from it in
            // between, so `remove` yields the element.
            return Ok(self.notifications.remove(position).expect("the indexed notification"));
        }
        let end = Instant::now() + deadline;
        loop {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(AcpClientError::Timeout {
                    what: format!("a $/cancel_request for {request_id}"),
                    deadline,
                });
            }
            let line = match self.lines.recv_timeout(remaining) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(AcpClientError::Timeout {
                        what: format!("a $/cancel_request for {request_id}"),
                        deadline,
                    });
                }
                Err(RecvTimeoutError::Disconnected) => return Err(AcpClientError::Closed),
            };
            match self.parse_line(&line)? {
                Frame::Notification { method, params } => {
                    let note = json!({ "method": method, "params": params });
                    if matches(&note) {
                        return Ok(note);
                    }
                    self.notifications.push_back(note);
                }
                Frame::Response { id, result } => self.early_responses.push((id, result)),
                Frame::Request { id, method, params } => self.answer_request(id, &method, &params)?,
            }
        }
    }
}
