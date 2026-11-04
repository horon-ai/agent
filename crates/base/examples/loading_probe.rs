//! Loading baseline probe: real save and session-preparation paths, measured by
//! stage.
//!
//! ```text
//! HORON_AGENT_HOME=<fixture home> \
//!   cargo run -p horon-agent-base --example loading_probe -- \
//!   --session perf-events-1000 --expect-events 1000 --skill-count 200
//! ```
//!
//! Prints one JSON object carrying every stage's wall time and the counters it
//! moved. `--help` prints the same contract in text.
//!
//! Exit codes:
//!
//! * `0` - every step ran and every counter was reported.
//! * `1` - counter or fixture mismatch: a stage's counts, the fixture shape, a
//!   required counter, or the unchanged log did not match what the run expected.
//! * `2` - preflight: bad arguments, an unresolvable or mismatched
//!   `HORON_AGENT_HOME`, a path the session store refuses as unsafe or
//!   non-private, or counters disabled by `HORON_AGENT_PERF_COUNTERS` - a
//!   disabled counter is "not measured", never "measured zero".
//!
//! What the measured run writes: loading a session publishes derived caches into
//! the measured session directory (`checkpoint.json`, `sparse-index.jsonl`,
//! `summary.json`, `facade-cache.json`, `.writer.lock`) and the catalog files
//! into `<home>/sessions/`. `events.jsonl` is not rewritten, and the report
//! carries its SHA-256 before and after. A second run on the same home therefore
//! reads less than the first, which is why `scripts/perf/bench_loading.py` keeps two
//! protocols apart: COLD samples each run on a fresh home, WARM discards three
//! warmups and then measures on one home. Samples from the two are never
//! compared, and the A/B comparison mode is WARM.

use anyhow::{Context, Result, bail};
use std::path::PathBuf;

#[path = "loading_probe/steps.rs"]
mod steps;

const USAGE: &str = "\
loading_probe - loading and preparation baseline probe

USAGE:
    HORON_AGENT_HOME=<fixture home> loading_probe --session <id> [options]

OPTIONS:
    --session <id>          session to load (required)
    --home <path>           expected home; must equal HORON_AGENT_HOME
    --expect-events <n>     records in the fixture log (default 0)
    --expect-messages <n>   messages the loading paths must return
                            (default: --expect-events - 1)
    --skill-count <n>       skills the catalog must discover (default 0)
    --scratch-messages <n>  messages the scratch save writes (default 8)
    -h, --help              print this help and exit 0

EXIT CODES:
    0  every step ran and every counter was reported
    1  counter or fixture mismatch
    2  preflight: bad arguments, home mismatch, a path the store refuses as
       unsafe or non-private, or measurement disabled by
       HORON_AGENT_PERF_COUNTERS
";

fn argument(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|value| value == "--help" || value == "-h")
}

fn parse(args: &[String]) -> Result<steps::Options> {
    let Some(session) = argument(args, "--session") else {
        bail!("--session <id> is required");
    };
    let home = match argument(args, "--home") {
        Some(home) => PathBuf::from(home),
        None => horon_agent_base::storage::horon_agent_dir().context("resolve HORON_AGENT_HOME")?,
    };
    let expect_events = argument(args, "--expect-events")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("--expect-events must be a whole number")?
        .unwrap_or(0);
    let expect_messages = argument(args, "--expect-messages")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("--expect-messages must be a whole number")?
        .unwrap_or_else(|| expect_events.saturating_sub(1));
    let skill_count = argument(args, "--skill-count")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("--skill-count must be a whole number")?
        .unwrap_or(0);
    let scratch_messages = argument(args, "--scratch-messages")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("--scratch-messages must be a whole number")?
        .unwrap_or(8);
    Ok(steps::Options {
        home,
        session,
        expect_events,
        expect_messages,
        skill_count,
        scratch_messages,
    })
}

fn exit_code(report: &serde_json::Value) -> i32 {
    report
        .get("exit_code")
        .and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(steps::EXIT_PREFLIGHT)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if wants_help(&args) {
        print!("{USAGE}");
        return;
    }
    let options = match parse(&args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("loading_probe: {error:#}");
            eprint!("{USAGE}");
            std::process::exit(steps::EXIT_PREFLIGHT);
        }
    };
    match steps::run(&options) {
        Ok(report) => {
            let code = exit_code(&report);
            match serde_json::to_string(&report) {
                Ok(text) => println!("{text}"),
                Err(error) => {
                    eprintln!("loading_probe: serialize report: {error}");
                    std::process::exit(steps::EXIT_PREFLIGHT);
                }
            }
            std::process::exit(code);
        }
        Err(error) => {
            eprintln!("loading_probe: {error:#}");
            std::process::exit(steps::EXIT_PREFLIGHT);
        }
    }
}
