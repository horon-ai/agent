use super::STREAM_KEEPALIVE_PONG_ID;
use crate::protocol::ServerEvent;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{self, MissedTickBehavior};

fn stream_keepalive_interval() -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        Duration::from_secs(30)
    }
}

pub(super) fn stream_keepalive_ticker() -> time::Interval {
    let interval = stream_keepalive_interval();
    let mut ticker = time::interval_at(time::Instant::now() + interval, interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticker
}

pub(super) fn send_stream_keepalive_mpsc(event_tx: &mpsc::UnboundedSender<ServerEvent>) {
    let _ = event_tx.send(ServerEvent::Pong {
        id: STREAM_KEEPALIVE_PONG_ID,
        native_ssh_protocol: None,
        capabilities: Vec::new(),
        user_connected_session_ids: None,
    });
}

/// Largest byte index `<= index` that is a UTF-8 char boundary in `text`.
/// Equivalent to the unstable `str::floor_char_boundary`, reimplemented so the
/// incremental marker scan can clamp its scan-window start onto a valid
/// boundary without re-scanning the whole accumulated response.
fn floor_char_boundary(text: &str, index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    let mut boundary = index;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

/// The wrapped-tool-call markers emitted by some models inside plain text.
const WRAP_TOOL_MARKERS: [&str; 2] = ["to=functions.", "+#+#"];

/// Find the first wrapped-tool-call marker in `accumulated`, scanning only the
/// newly appended `delta` plus a short overlap from the previous tail (so a
/// marker straddling the append boundary is still found).
///
/// This avoids re-scanning the entire accumulated response on every streamed
/// delta, which was O(response) per token and O(response^2) over a full answer.
pub(super) fn find_wrap_marker_incremental(accumulated: &str, appended_len: usize) -> Option<usize> {
    let max_marker_len = WRAP_TOOL_MARKERS.iter().map(|marker| marker.len()).max().unwrap_or(0);
    let scan_start = accumulated
        .len()
        .saturating_sub(appended_len + max_marker_len.saturating_sub(1));
    let scan_start = floor_char_boundary(accumulated, scan_start);
    let window = &accumulated[scan_start..];
    WRAP_TOOL_MARKERS
        .iter()
        .filter_map(|marker| window.find(marker))
        .min()
        .map(|rel_idx| scan_start + rel_idx)
}
