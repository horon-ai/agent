use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;

const CANCEL_GRACE: Duration = Duration::from_secs(1);

pub(in crate::cli) fn parse_run_duration(raw: &str) -> Result<Duration, &'static str> {
    let invalid = "expected a positive, representable duration in seconds";
    let seconds = raw.parse::<f64>().map_err(|_| invalid)?;
    let duration = Duration::try_from_secs_f64(seconds).map_err(|_| invalid)?;
    if duration.is_zero() || std::time::Instant::now().checked_add(duration).is_none() {
        return Err(invalid);
    }
    Ok(duration)
}

#[derive(Debug)]
pub(in crate::cli) struct RunDeadlineExceeded(Duration);

impl fmt::Display for RunDeadlineExceeded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "run exceeded its --max-time limit ({:?})", self.0)
    }
}

impl std::error::Error for RunDeadlineExceeded {}

impl RunDeadlineExceeded {
    pub(super) fn report(&self, session_id: &str, json: bool, ndjson: bool) -> io::Result<()> {
        if json || ndjson {
            let report = serde_json::json!({
                "type": "error",
                "code": "deadline_exceeded",
                "session_id": session_id,
                "message": self.to_string(),
                "max_time_seconds": self.0.as_secs_f64(),
            });
            writeln!(io::stdout().lock(), "{report}")
        } else {
            writeln!(io::stderr().lock(), "{self}")
        }
    }
}

pub(super) async fn run_with_deadline<T>(
    duration: Duration,
    elapsed: &AtomicBool,
    cancel: impl FnOnce(),
    run: impl Future<Output = Result<T>>,
) -> Result<T> {
    let mut run = std::pin::pin!(run);
    if let Ok(result) = tokio::time::timeout(duration, &mut run).await {
        return result;
    }
    elapsed.store(true, Ordering::Relaxed);
    cancel();
    // A provider or tool that ignores cancellation cannot delay the CLI past
    // this grace period. Dropping the future releases its stream and guards.
    drop(tokio::time::timeout(CANCEL_GRACE, run).await);
    Err(RunDeadlineExceeded(duration).into())
}
