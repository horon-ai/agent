//! `horon-agent runtime-state` - inspect and settle durable pending inputs.
//!
//! The subcommands exist because an undetermined input is a decision only the
//! operator can make: a turn that started and never recorded a terminal row may
//! or may not have reached the provider, and re-running it silently could repeat
//! a side effect. `resolve-input` is that decision, and `export-legacy` is the
//! rollback that hands the queue back to an older build.

use anyhow::{Result, bail};

use super::args::{RuntimeStateAction, RuntimeStateCommand};

pub(crate) fn run(command: RuntimeStateCommand) -> Result<()> {
    match command.action {
        RuntimeStateAction::Show { session, json } => run_show(&session, json),
        RuntimeStateAction::ResolveInput { session, input, action } => run_resolve_input(&session, &input, &action),
        RuntimeStateAction::ExportLegacy { session, json } => run_export_legacy(&session, json),
    }
}

fn open(session: &str) -> Result<crate::durable_input::DurableInputStore> {
    crate::durable_input::DurableInputStore::open(session)
}

fn run_show(session: &str, json: bool) -> Result<()> {
    let mut store = open(session)?;
    let state = store.state()?.clone();
    let mut live = store.live_messages()?;
    live.sort_by_key(|message| message.input_id.clone());
    if json {
        let inputs: Vec<serde_json::Value> = state
            .inputs
            .iter()
            .map(|(id, entry)| {
                serde_json::json!({
                    "input_id": id,
                    "phase": format!("{:?}", entry.phase).to_lowercase(),
                    "client_id": entry.accepted.client_id,
                    "source": entry.accepted.source,
                    "urgent": entry.accepted.urgent,
                    "seq": entry.seq,
                    "run_id": entry.run_id,
                    "note": entry.note,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "session": session,
                "inputs": inputs,
            }))?
        );
        return Ok(());
    }
    if state.inputs.is_empty() {
        println!("No live inputs for session {session}.");
        return Ok(());
    }
    println!("Live inputs for session {session}:");
    for (id, entry) in &state.inputs {
        println!(
            "  {id}  {:?}  source={} urgent={} client_id={}",
            entry.phase,
            entry.accepted.source,
            entry.accepted.urgent,
            entry.accepted.client_id.as_deref().unwrap_or("-")
        );
        if let Some(note) = &entry.note {
            println!("      note: {note}");
        }
    }
    let undetermined = state
        .undetermined_inputs()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if !undetermined.is_empty() {
        println!();
        println!(
            "{} input(s) have no settled outcome: {}",
            undetermined.len(),
            undetermined.join(", ")
        );
        println!(
            "Settle each with: horon-agent runtime-state resolve-input --session {session} --input <id> --action retry|cancel"
        );
    }
    Ok(())
}

fn run_resolve_input(session: &str, input_id: &str, action: &str) -> Result<()> {
    let Some(action) = crate::durable_input::ResolveAction::parse(action) else {
        bail!("unknown action {action:?}; use retry or cancel");
    };
    let mut store = open(session)?;
    let resolved = store.resolve(input_id, action, |message_id| {
        remove_transcript_copy(session, message_id)
    })?;
    println!(
        "Resolved input {} (was {:?}) with {}.",
        resolved.input_id,
        resolved.previous_phase,
        action.as_str()
    );
    if let Some(fresh) = &resolved.requeued_as {
        println!("The message is queued again as input {fresh}.");
    }
    if resolved.cleaned_injected_copy {
        println!("The previously injected transcript copy was removed.");
    }
    if let Some(warning) = &resolved.warning {
        println!("Note: {warning}");
    }
    if action == crate::durable_input::ResolveAction::Retry {
        println!("A retry runs the message again. Any tool the earlier attempt already ran may run a second time.");
    }
    Ok(())
}

fn run_export_legacy(session: &str, json: bool) -> Result<()> {
    let mut store = open(session)?;
    match store.export_legacy()? {
        Ok(export) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session": session,
                        "exported": export.exported,
                        "path": export.path.display().to_string(),
                    }))?
                );
            } else {
                println!(
                    "Exported {} pending input(s) to {}.",
                    export.exported,
                    export.path.display()
                );
                println!("Only now is it safe to run an older build against this home.");
            }
            Ok(())
        }
        Err(refused) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session": session,
                        "refused": true,
                        "reasons": refused.reasons,
                    }))?
                );
            } else {
                println!("Export refused; the session is not quiescent:");
                for reason in &refused.reasons {
                    println!("  - {reason}");
                }
            }
            bail!("export-legacy refused for session {session}")
        }
    }
}

/// Remove one injected message from the stored conversation.
///
/// The transcript lives in the session's own event log, so this loads the
/// session, removes exactly that message id, and saves. A missing message is
/// reported to the caller rather than treated as success.
fn remove_transcript_copy(session_id: &str, message_id: &str) -> bool {
    let mut session = match crate::session::Session::load(session_id) {
        Ok(session) => session,
        Err(error) => {
            crate::logging::warn(&format!(
                "RUNTIME_STATE_RESOLVE_LOAD_FAILED session={session_id} error={error}"
            ));
            return false;
        }
    };
    if session.remove_messages_by_id(&[message_id.to_string()]) == 0 {
        return false;
    }
    match session.save() {
        Ok(()) => true,
        Err(error) => {
            crate::logging::warn(&format!(
                "RUNTIME_STATE_RESOLVE_SAVE_FAILED session={session_id} error={error}"
            ));
            false
        }
    }
}
