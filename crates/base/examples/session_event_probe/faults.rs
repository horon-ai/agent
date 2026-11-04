use super::*;

#[cfg(feature = "test-support")]
pub(super) fn retry() -> Result<()> {
    let mut session = Session::create_with_id("public_retry".into(), None, None);
    let mut armed = true;
    session.set_event_io_hook(Box::new(move |point| {
        if armed && point == store::IoPoint::LogBodyWritten {
            armed = false;
            return Err(std::io::Error::other("public probe partial append"));
        }
        Ok(())
    }))?;
    session.add_message(Role::User, text("same event ids"));
    ensure!(session.save().is_err());
    let reserved = session.pending_event_ids();
    session.save()?;
    let lines = std::fs::read_to_string(session.event_log_path()?)?;
    let events = lines
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for id in &reserved {
        ensure!(events.iter().filter(|e| e["event_id"] == *id).count() == 1);
    }
    ensure!(Session::load(&session.id)?.history_messages()?.len() == 1);
    println!(
        "{}",
        json!({"operation":"retry","same_ids":reserved,"false_ack":false,"messages":1})
    );
    Ok(())
}

#[cfg(feature = "test-support")]
pub(super) fn drain() -> Result<()> {
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut session = Session::create_with_id("public_drain".into(), None, None);
    let mut first = true;
    session.set_event_io_hook(Box::new(move |point| {
        if first && point == store::IoPoint::LogDataSync {
            first = false;
            ready_tx.send(()).map_err(std::io::Error::other)?;
            release_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .map_err(std::io::Error::other)?;
        }
        Ok(())
    }))?;
    session.add_message(Role::User, text("drained commit"));
    let saving = std::thread::spawn(move || session.save());
    ready_rx.recv_timeout(std::time::Duration::from_secs(10))?;
    ensure!(!horon_agent_base::session::drain_saves_for_shutdown(
        std::time::Duration::ZERO
    ));
    release_tx.send(())?;
    saving.join().map_err(|_| anyhow::anyhow!("save thread failed"))??;
    ensure!(horon_agent_base::session::drain_saves_for_shutdown(
        std::time::Duration::from_secs(1)
    ));
    let mut session = Session::load("public_drain")?;
    ensure!(session.history_messages()?.len() == 1 && session.save().is_err());
    println!(
        "{}",
        json!({"operation":"drain","inflight_timeout":true,"durable_after_release":true,"new_save_refused":true})
    );
    Ok(())
}

#[cfg(not(feature = "test-support"))]
pub(super) fn retry() -> Result<()> {
    anyhow::bail!("test-support feature required")
}
#[cfg(not(feature = "test-support"))]
pub(super) fn drain() -> Result<()> {
    anyhow::bail!("test-support feature required")
}
