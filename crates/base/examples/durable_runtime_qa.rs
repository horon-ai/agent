//! Manual QA for the durable runtime journal.
//!
//! Runs without a terminal or a PTY: it writes real rows through the public
//! store API, truncates the final row mid-JSON, reopens the session to prove the
//! valid prefix replayed with the raw tail preserved, and finally proves a
//! corrupt complete row is refused. Exit code 0 means every outcome held.
//!
//! Usage: `durable_runtime_qa <journal-root> <session-id>`

use std::io::Write;
use std::path::Path;

use horon_agent_base::durable_runtime::{
    DurableRuntimeStore, InputAccepted, InputCommitted, InputPhase, InputStarted, JournalErrorKind, Payload,
    RunStarted, RunStopped, StopCause, new_id,
};
use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: durable_runtime_qa <journal-root> <session-id>");
        std::process::exit(2);
    }
    let root = Path::new(&args[1]);
    let session = &args[2];
    match run(root, session) {
        Ok(report) => {
            println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
            println!("QA RESULT: PASS");
        }
        Err(failure) => {
            println!("{}", serde_json::to_string_pretty(&failure).unwrap_or_default());
            println!("QA RESULT: FAIL");
            std::process::exit(1);
        }
    }
}

fn accepted(content: &str) -> Payload {
    Payload::InputAccepted(InputAccepted {
        client_id: Some("qa-client".to_string()),
        content: content.to_string(),
        images: Vec::new(),
        urgent: false,
        source: "user".to_string(),
    })
}

fn run(root: &Path, session: &str) -> Result<Value, Value> {
    let mut written = Vec::new();
    let mut store =
        DurableRuntimeStore::open_in(root, session).map_err(|error| failure(format!("open {session}: {error}")))?;
    let input = new_id();
    let run_id = new_id();

    let rows: [(&str, Payload); 5] = [
        ("input.accepted", accepted("qa input")),
        (
            "input.started",
            Payload::InputStarted(InputStarted {
                run_id: Some(run_id.clone()),
            }),
        ),
        (
            "run.started",
            Payload::RunStarted(RunStarted {
                input_id: Some(input.clone()),
            }),
        ),
        (
            "run.stopped",
            Payload::RunStopped(RunStopped {
                cause: StopCause::Completed,
                input_id: Some(input.clone()),
            }),
        ),
        (
            "input.committed",
            Payload::InputCommitted(InputCommitted { run_id: Some(run_id) }),
        ),
    ];
    for (kind, payload) in rows {
        let record = store
            .append(&input, payload)
            .map_err(|error| failure(format!("append {kind}: {error}")))?;
        written.push(json!({"kind": kind, "seq": record.seq}));
    }
    let path = store.journal_path().to_path_buf();
    let durable_len = store.journal_bytes();
    drop(store);

    let bytes = std::fs::read(&path).map_err(|error| failure(format!("read journal: {error}")))?;
    if bytes.len() as u64 != durable_len {
        return Err(failure(format!(
            "journal is {} bytes but the store reported {durable_len}",
            bytes.len()
        )));
    }
    let cut = bytes.len() - 9;
    let truncated = bytes[..cut].to_vec();
    let tail_start = truncated
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|position| position + 1)
        .unwrap_or(0);
    let torn = truncated[tail_start..].to_vec();
    std::fs::write(&path, &truncated).map_err(|error| failure(format!("truncate: {error}")))?;

    let mut repaired = DurableRuntimeStore::open_in(root, session)
        .map_err(|error| failure(format!("reopen after truncation: {error}")))?;
    let state = repaired
        .snapshot()
        .map_err(|error| failure(format!("replay: {error}")))?;
    let recoveries = repaired.recoveries().to_vec();
    if recoveries.len() != 1 {
        return Err(failure(format!(
            "expected exactly one recovery, found {}",
            recoveries.len()
        )));
    }
    let report = &recoveries[0];
    let preserved =
        std::fs::read(&report.preserved_tail).map_err(|error| failure(format!("read preserved tail: {error}")))?;
    if preserved != torn {
        return Err(failure(
            "the preserved tail differs from the bytes cut from the journal",
        ));
    }
    if serde_json::from_slice::<Value>(&preserved).is_ok() {
        return Err(failure("the preserved tail is complete JSON; the cut was not mid-row"));
    }
    let replayed = state
        .inputs
        .get(&input)
        .ok_or_else(|| failure("the replayed prefix lost the accepted input"))?;
    if replayed.phase != InputPhase::Started {
        return Err(failure(format!(
            "the replayed prefix has phase {:?}, expected the started prefix before the lost commit",
            replayed.phase
        )));
    }
    if state.input_receipts.contains_key(&input) {
        return Err(failure("the dropped commit row was replayed anyway"));
    }
    let replayed_seq = repaired.last_seq();
    let next = repaired
        .append(&new_id(), accepted("after recovery"))
        .map_err(|error| failure(format!("append after recovery: {error}")))?;
    drop(repaired);

    let corrupt_session = format!("{session}-corrupt-{}", std::process::id());
    let mut store = DurableRuntimeStore::open_in(root, &corrupt_session)
        .map_err(|error| failure(format!("open {corrupt_session}: {error}")))?;
    store
        .append(&new_id(), accepted("will be corrupted"))
        .map_err(|error| failure(format!("append before corruption: {error}")))?;
    let corrupt_path = store.journal_path().to_path_buf();
    drop(store);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&corrupt_path)
        .map_err(|error| failure(format!("open corrupt journal: {error}")))?;
    file.write_all(b"{\"v\":1,\"seq\":3,\"session_id\":\"x\n")
        .map_err(|error| failure(format!("write corrupt row: {error}")))?;
    drop(file);
    let rejection = match DurableRuntimeStore::open_in(root, &corrupt_session) {
        Ok(_) => return Err(failure("a corrupt complete row was accepted")),
        Err(error) => error,
    };
    if rejection.kind() != JournalErrorKind::Corrupt {
        return Err(failure(format!(
            "corrupt row was rejected as {}, expected corrupt",
            rejection.kind().as_str()
        )));
    }

    Ok(json!({
        "journal": path.display().to_string(),
        "written": written,
        "durable_bytes_before_truncation": durable_len,
        "truncated_to_bytes": cut,
        "torn_tail_bytes": torn.len(),
        "recovery": {
            "source_generation": report.source_generation,
            "preserved_tail": report.preserved_tail.display().to_string(),
            "preserved_bytes": report.preserved_bytes,
            "rewritten_seq": report.rewritten_seq,
        },
        "replayed": {
            "last_seq_after_repair": replayed_seq,
            "input_phase": "started",
            "commit_lost_with_the_tail": true,
            "append_after_recovery_seq": next.seq,
        },
        "corrupt_row": {
            "journal": corrupt_path.display().to_string(),
            "rejection_kind": rejection.kind().as_str(),
            "message": rejection.message(),
        },
    }))
}

fn failure(message: impl Into<String>) -> Value {
    json!({"error": message.into()})
}
