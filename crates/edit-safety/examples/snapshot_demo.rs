//! Stores two revisions of one file, then a tiny-budget store whose second
//! commit evicts its first unit, so the budget rule can be read off real
//! output.

use horon_agent_edit_safety::{BlobId, FileContent, FileState, SnapshotStore};

fn recorded_after(store: &SnapshotStore, tool_call_id: &str) -> Option<String> {
    let unit = store.history().find(|unit| unit.tool_call_id == tool_call_id)?;
    match &unit.files.first()?.after {
        FileState::Present(id) => Some(id.to_string()),
        _ => None,
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut store = SnapshotStore::new();
    println!("default budget: {} bytes", store.byte_budget());

    let first = b"line one\nline two\n";
    let second = b"line one\nline two changed\n";

    store.begin_file(
        "call-1",
        "notes.txt",
        FileContent::Absent,
        FileContent::Bytes(first.to_vec()),
    )?;
    store.commit("call-1")?;
    println!(
        "call-1 recorded blob {} (stored: {})",
        recorded_after(&store, "call-1").unwrap_or_default(),
        store.blob(&BlobId::of(first)).is_some()
    );

    store.begin_file(
        "call-2",
        "notes.txt",
        FileContent::Bytes(first.to_vec()),
        FileContent::Bytes(second.to_vec()),
    )?;
    store.commit("call-2")?;
    println!(
        "call-2 recorded blob {} (stored: {})",
        recorded_after(&store, "call-2").unwrap_or_default(),
        store.blob(&BlobId::of(second)).is_some()
    );
    println!(
        "byte usage after two revisions: {} of {}",
        store.byte_usage(),
        store.byte_budget()
    );
    println!("history: {:?}", store.history_ids());

    let mut tiny = SnapshotStore::with_byte_budget(8);
    println!("\ntiny budget: {} bytes", tiny.byte_budget());
    for (tool_call_id, bytes) in [("call-a", &b"aaaa"[..]), ("call-b", &b"bbbbbbbb"[..])] {
        tiny.begin_file(
            tool_call_id,
            format!("{tool_call_id}.txt"),
            FileContent::Absent,
            FileContent::Bytes(bytes.to_vec()),
        )?;
        let outcome = tiny.commit(tool_call_id)?;
        println!(
            "{tool_call_id}: {} bytes, evicted {:?}, usage {} bytes",
            bytes.len(),
            outcome.evicted_units,
            outcome.byte_usage
        );
    }
    println!("history after eviction: {:?}", tiny.history_ids());

    Ok(())
}
